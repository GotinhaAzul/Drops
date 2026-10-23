//! Regression tests for live-instance resolution.
//!
//! A crashed Hyprland can leave `.socket.sock` / `.socket2.sock` files behind.
//! `hyprctl instances` is authoritative, so those leftovers must not pin the
//! dead inherited signature once a new sole instance is live. The tests use a
//! fake `hyprctl` so they run without a compositor.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Environment variables and the fake `hyprctl` are process-global.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct Fixture {
    bin: PathBuf,
    runtime: PathBuf,
    /// Set whenever the fake compositor's socket directory is created.
    stale: PathBuf,
    fresh: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!(
            "buddy-anchor-session-{name}-{}-{nanos}",
            std::process::id()
        ));
        let bin = root.join("bin");
        let runtime = root.join("run");
        fs::create_dir_all(&bin).expect("fake bin dir");
        fs::create_dir_all(&runtime).expect("fake runtime dir");

        let stale = runtime.join("hypr").join("stale");
        let fresh = runtime.join("hypr").join("fresh");

        let fixture = Fixture {
            bin,
            runtime,
            stale,
            fresh,
        };
        fixture.write_fake_hyprctl();
        fixture
    }

    /// A fake `hyprctl` that lists a stale instance on its first `instances`
    /// call when asked, and otherwise reports a sole live "fresh" instance.
    /// Queries against any signature other than "fresh" fail like a dead
    /// compositor would.
    fn write_fake_hyprctl(&self) {
        let script = r#"#!/bin/sh
if [ "$1" = "-j" ] && [ "$2" = "instances" ]; then
    count=$(cat "$FAKE_INSTANCE_COUNTER" 2>/dev/null || echo 0)
    count=$((count + 1))
    echo "$count" > "$FAKE_INSTANCE_COUNTER"
    if [ "$FAKE_INSTANCES_STALE_FIRST" = "1" ] && [ "$count" -le 1 ]; then
        printf '[{"instance":"stale"}]'
    else
        printf '[{"instance":"fresh"}]'
    fi
    exit 0
fi
if [ "$HYPRLAND_INSTANCE_SIGNATURE" != "fresh" ]; then
    echo "instance is gone" >&2
    exit 1
fi
case "$2" in
    clients|workspaces|monitors) printf '[]' ;;
    *) exit 1 ;;
esac
"#;
        let path = self.bin.join("hyprctl");
        fs::write(&path, script).expect("fake hyprctl");
        let mut permissions = fs::metadata(&path)
            .expect("fake hyprctl metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake hyprctl");
    }

    /// Point the process at this fixture for the duration of the guard.
    ///
    /// `stale_first` makes the first `hyprctl instances` call report only the
    /// dead inherited instance, which exercises the query/connect retry.
    fn install(&self, signature: &str, stale_first: bool) -> EnvGuard {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let counter = self.runtime.join("instances-count");
        EnvGuard::set(&[
            ("PATH", path),
            ("HYPRLAND_INSTANCE_SIGNATURE", signature.to_string()),
            ("XDG_RUNTIME_DIR", self.runtime.display().to_string()),
            (
                "FAKE_INSTANCES_STALE_FIRST",
                if stale_first { "1" } else { "0" }.to_string(),
            ),
            ("FAKE_INSTANCE_COUNTER", counter.display().to_string()),
        ])
    }

    /// Leave the socket files a crashed compositor would leave behind.
    fn stale_socket_files(&self) -> &Path {
        fs::create_dir_all(&self.stale).expect("stale socket dir");
        fs::write(self.stale.join(".socket.sock"), b"").expect("stale control socket");
        fs::write(self.stale.join(".socket2.sock"), b"").expect("stale event socket");
        &self.stale
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let root = self.runtime.parent().unwrap_or(&self.runtime);
        let _ = fs::remove_dir_all(root);
    }
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn set(values: &[(&'static str, String)]) -> Self {
        let mut saved = Vec::with_capacity(values.len());
        for (key, value) in values {
            saved.push((*key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        EnvGuard { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
async fn stale_socket_files_do_not_pin_the_attached_instance() {
    let _lock = ENV_LOCK.lock().await;
    let fixture = Fixture::new("stale-socket");
    // The old signature still has both socket files, but no compositor.
    fixture.stale_socket_files();
    let _env = fixture.install("stale", false);

    assert!(
        buddy_anchor::desktop_available().is_ok(),
        "the live list must win over a stale socket file"
    );
    let snapshot = buddy_anchor::snapshot().await.expect("snapshot recovers");
    assert!(snapshot.clients.is_empty());
}

#[tokio::test]
async fn failed_stale_query_retries_the_sole_live_instance() {
    let _lock = ENV_LOCK.lock().await;
    let fixture = Fixture::new("query-retry");
    fixture.stale_socket_files();
    // The first resolve sees only the dead attached instance and its query
    // fails; the retry must discover the sole live instance instead.
    let _env = fixture.install("stale", true);

    let snapshot = buddy_anchor::snapshot()
        .await
        .expect("snapshot retries against the fresh instance");
    assert!(snapshot.clients.is_empty());
}

#[tokio::test]
async fn subscribe_rediscovers_after_a_stale_event_socket() {
    let _lock = ENV_LOCK.lock().await;
    let fixture = Fixture::new("subscribe-rediscover");
    let stale_dir = fixture.stale_socket_files().to_path_buf();
    assert!(
        stale_dir.join(".socket2.sock").exists(),
        "the dead signature keeps an event socket file"
    );

    fs::create_dir_all(&fixture.fresh).expect("fresh socket dir");
    let listener =
        UnixListener::bind(fixture.fresh.join(".socket2.sock")).expect("fresh event socket");
    let _env = fixture.install("stale", true);

    let (events, mut signals) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { buddy_anchor::subscribe(events, cancel).await }
    });

    // The stale event socket file exists but cannot be connected to; the next
    // attempt must discover and connect to the fresh instance.
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("subscribe rediscovers the live instance")
        .expect("fresh connection");

    // Every successful connection signals a refresh, including the first.
    tokio::time::timeout(Duration::from_secs(2), signals.recv())
        .await
        .expect("initial refresh")
        .expect("channel open");

    stream
        .write_all(b"workspace>>1\n")
        .await
        .expect("write event");
    stream.flush().await.expect("flush");
    tokio::time::timeout(Duration::from_secs(2), signals.recv())
        .await
        .expect("event after discovery")
        .expect("channel open");

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("subscription exits on cancel")
        .expect("task join")
        .expect("subscription result");
}
