//! Read-only Hyprland integration: `hyprctl` queries and socket2 events.
//!
//! This module only observes the compositor. It never runs `hyprctl dispatch`
//! and never writes desktop configuration.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::error::AnchorError;
use crate::model::{Client, CursorPos, Monitor, Snapshot, Workspace};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_SOCKET: &str = ".socket2.sock";
const CONTROL_SOCKET: &str = ".socket.sock";

/// First delay before reconnecting to a lost event socket.
const RECONNECT_MIN: Duration = Duration::from_millis(200);
/// Upper bound for the reconnect backoff.
const RECONNECT_MAX: Duration = Duration::from_secs(5);
/// A connection that lasted this long resets the backoff.
const STABLE_CONNECTION: Duration = Duration::from_secs(1);

/// Events the daemon must react to by re-querying state.
///
/// The payloads are ignored on purpose: the repository only asks the daemon to
/// refresh anchors, and a full query is always more accurate than parsing
/// partial event payloads.
pub const TRACKED_EVENTS: [&str; 29] = [
    "openwindow",
    "closewindow",
    "movewindow",
    "movewindowv2",
    "workspace",
    "workspacev2",
    "createworkspace",
    "createworkspacev2",
    "destroyworkspace",
    "destroyworkspacev2",
    "moveworkspace",
    "moveworkspacev2",
    "renameworkspace",
    "focusedmon",
    "activespecial",
    "activespecialv2",
    "fullscreen",
    "changefloatingmode",
    "pin",
    "windowtitle",
    "windowtitlev2",
    "togglefloating",
    "togglegroup",
    "moveintogroup",
    "moveoutofgroup",
    "monitoradded",
    "monitoraddedv2",
    "monitorremoved",
    "monitorremovedv2",
];

/// The Hyprland instance signature, when this process inherited one.
pub fn instance_signature() -> Option<String> {
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .ok()
        .map(|signature| signature.trim().to_string())
        .filter(|signature| !signature.is_empty())
}

/// Directories where Hyprland may have created its sockets.
pub fn socket_dirs(signature: &str, xdg_runtime_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(runtime_dir) = xdg_runtime_dir {
        dirs.push(runtime_dir.join("hypr").join(signature));
    }
    dirs.push(PathBuf::from("/tmp/hypr").join(signature));
    dirs
}

/// Verify a live Hyprland session is observable.
///
/// `hyprctl instances` is authoritative because it only lists instances whose
/// processes are still alive: a stale `.socket.sock` left behind by a crash
/// cannot pin a dead signature. The inherited signature is preferred when it
/// is among the live instances; otherwise a sole live instance is adopted and
/// multiple live instances are never chosen arbitrarily.
///
/// When the instance list is unavailable (no `hyprctl`, or a test fixture with
/// no compositor), the inherited socket-file check keeps the headless path
/// working. This intentionally fails on a headless machine instead of
/// returning made-up state. Callers can surface the error to the user.
pub fn ensure_desktop() -> Result<String, AnchorError> {
    let attached = instance_signature();
    if let Some(instances) = query_instances_blocking() {
        let live = live_signatures(&instances);
        if !live.is_empty() {
            return select_live_signature(attached.as_deref(), &live);
        }
    }
    let xdg_runtime_dir = runtime_dir();
    select_socket_signature(attached.as_deref(), |signature| {
        control_socket(signature, xdg_runtime_dir.as_deref()).is_some()
    })
}

/// The runtime directory Hyprland places per-instance sockets in.
fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

/// A session's control socket, when it exists in one of the known locations.
fn control_socket(signature: &str, xdg_runtime_dir: Option<&Path>) -> Option<PathBuf> {
    socket_dirs(signature, xdg_runtime_dir)
        .into_iter()
        .map(|dir| dir.join(CONTROL_SOCKET))
        .find(|socket| socket.exists())
}

/// One entry from `hyprctl -j instances`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct InstanceInfo {
    /// Newer releases report the bare instance signature here; older ones may
    /// report the socket directory path.
    #[serde(default)]
    instance: String,
    #[serde(default)]
    signature: String,
}

impl InstanceInfo {
    fn signature(&self) -> Option<&str> {
        let explicit = self.signature.trim();
        if !explicit.is_empty() {
            return Some(explicit);
        }

        let instance = self.instance.trim().trim_end_matches('/');
        if instance.is_empty() {
            return None;
        }
        let tail = instance.rsplit('/').next().unwrap_or(instance);
        (!tail.is_empty()).then_some(tail)
    }
}

/// Parse `hyprctl -j instances`, accepting both the array shape and a wrapped
/// `{"instances": [...]}` object.
fn parse_instances(bytes: &[u8]) -> Option<Vec<InstanceInfo>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let items = match value {
        Value::Array(items) => items,
        Value::Object(mut map) => match map.remove("instances")? {
            Value::Array(items) => items,
            _ => return None,
        },
        _ => return None,
    };
    items
        .into_iter()
        .map(|item| serde_json::from_value(item).ok())
        .collect()
}

fn live_signatures(instances: &[InstanceInfo]) -> Vec<String> {
    let mut signatures = BTreeSet::new();
    for instance in instances {
        if let Some(signature) = instance.signature() {
            signatures.insert(signature.to_string());
        }
    }
    signatures.into_iter().collect()
}

/// Choose an instance from an authoritative live list.
///
/// The attached instance wins only when `hyprctl` says it is alive. If it is
/// not, a sole live instance is adopted; multiple live instances are an error.
fn select_live_signature(
    attached: Option<&str>,
    live_instances: &[String],
) -> Result<String, AnchorError> {
    if let Some(attached) =
        attached.filter(|attached| live_instances.iter().any(|signature| signature == attached))
    {
        return Ok(attached.to_string());
    }

    match live_instances {
        [signature] => Ok(signature.clone()),
        [] => Err(AnchorError::DesktopUnavailable(
            "no live Hyprland instance was reported by `hyprctl instances`".to_string(),
        )),
        signatures => Err(AnchorError::DesktopUnavailable(format!(
            "{} and multiple Hyprland instances are live ({}); refusing to guess which one to attach to",
            match attached {
                Some(signature) => format!("attached instance {signature} is not live"),
                None => "HYPRLAND_INSTANCE_SIGNATURE is not set".to_string(),
            },
            signatures.join(", ")
        ))),
    }
}

/// Fallback selection when no live-instance list is available.
///
/// This trusts the inherited socket file and exists for fixtures and older
/// environments where `hyprctl instances` cannot report anything. Query
/// failures still trigger a fresh re-discovery attempt at the call site.
fn select_socket_signature(
    attached: Option<&str>,
    socket_exists: impl Fn(&str) -> bool,
) -> Result<String, AnchorError> {
    match attached {
        Some(signature) if socket_exists(signature) => Ok(signature.to_string()),
        Some(signature) => Err(AnchorError::DesktopUnavailable(format!(
            "no live Hyprland socket for instance {signature} (it may have restarted) and no live instance was reported by `hyprctl instances`"
        ))),
        None => Err(AnchorError::DesktopUnavailable(
            "HYPRLAND_INSTANCE_SIGNATURE is not set and no live instance was reported by `hyprctl instances`"
                .to_string(),
        )),
    }
}

fn query_instances_blocking() -> Option<Vec<InstanceInfo>> {
    let output = std::process::Command::new("hyprctl")
        .args(["-j", "instances"])
        // Enumerate every session instead of whatever the stale inherited
        // signature points at. Only the child's environment is changed.
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_instances(&output.stdout)
}

async fn query_instances() -> Result<Vec<InstanceInfo>> {
    let command = Command::new("hyprctl")
        .args(["-j", "instances"])
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(QUERY_TIMEOUT, command)
        .await
        .map_err(|_| AnchorError::Hyprctl("hyprctl -j instances timed out".to_string()))?
        .context("failed to run hyprctl (is it installed and on PATH?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AnchorError::Hyprctl(format!(
            "`hyprctl -j instances` exited with {}: {}",
            output.status,
            stderr.trim()
        ))
        .into());
    }

    parse_instances(&output.stdout)
        .ok_or_else(|| AnchorError::Hyprctl("could not parse `hyprctl -j instances`".into()).into())
}

/// Resolve the instance for a single command.
///
/// The live list from `hyprctl instances` is consulted first on every command,
/// so a stale socket file left by a crash cannot pin the inherited signature
/// even when a new instance is running. If the list is unavailable or empty,
/// the inherited socket file is the fallback.
async fn resolve_signature() -> Result<String, AnchorError> {
    let attached = instance_signature();
    if let Ok(instances) = query_instances().await {
        let live = live_signatures(&instances);
        if !live.is_empty() {
            return select_live_signature(attached.as_deref(), &live);
        }
    }

    let xdg_runtime_dir = runtime_dir();
    select_socket_signature(attached.as_deref(), |signature| {
        control_socket(signature, xdg_runtime_dir.as_deref()).is_some()
    })
}

/// Resolve after a failed connection to the attached instance.
///
/// When exactly one other instance is live, prefer it over the inherited
/// signature that just failed. Anything else falls back to normal resolution
/// so a transient failure does not turn into an arbitrary choice.
async fn resolve_signature_preferring_discovery() -> Result<String, AnchorError> {
    if let Ok(instances) = query_instances().await {
        let live = live_signatures(&instances);
        if let [only] = live.as_slice() {
            return Ok(only.clone());
        }
    }
    resolve_signature().await
}

/// Resolve again after a query failure. Returns a new signature only when it
/// differs from the failed one, so a retry cannot loop.
async fn retry_signature(failed: &str) -> Option<String> {
    let retry = resolve_signature().await.ok()?;
    (retry != failed).then_some(retry)
}

/// Path to a session's event socket inside the known socket directories.
fn event_socket_for(
    signature: &str,
    xdg_runtime_dir: Option<&Path>,
) -> Result<PathBuf, AnchorError> {
    for dir in socket_dirs(signature, xdg_runtime_dir) {
        let candidate = dir.join(EVENT_SOCKET);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(AnchorError::DesktopUnavailable(format!(
        "Hyprland event socket {EVENT_SOCKET} was not found for instance {signature}"
    )))
}

/// Path to the event socket for one connection attempt.
async fn resolve_event_socket_async(prefer_discovery: bool) -> Result<PathBuf, AnchorError> {
    let signature = if prefer_discovery {
        resolve_signature_preferring_discovery().await?
    } else {
        resolve_signature().await?
    };
    event_socket_for(&signature, runtime_dir().as_deref())
}

/// Parse the event name out of a socket2 line such as
/// `movewindow>>0x55d0,0,100`.
pub fn event_name(line: &str) -> &str {
    let line = line.trim();
    line.split(">>").next().unwrap_or(line).trim()
}

/// Whether a socket2 line changes any anchor-relevant state.
pub fn is_tracked_event(line: &str) -> bool {
    TRACKED_EVENTS.contains(&event_name(line))
}

async fn hyprctl_for(signature: &str, args: &[&str]) -> Result<Vec<u8>> {
    let command = Command::new("hyprctl")
        .args(args)
        // Target the instance resolved for this command without mutating the
        // daemon's own environment.
        .env("HYPRLAND_INSTANCE_SIGNATURE", signature)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(QUERY_TIMEOUT, command)
        .await
        .map_err(|_| AnchorError::Hyprctl(format!("hyprctl {} timed out", args.join(" "))))?
        .context("failed to run hyprctl (is it installed and on PATH?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(AnchorError::Hyprctl(format!(
            "`hyprctl {}` exited with {}: {detail}",
            args.join(" "),
            output.status
        ))
        .into());
    }

    Ok(output.stdout)
}

async fn query<T: DeserializeOwned>(signature: &str, subcommand: &str) -> Result<T> {
    let stdout = hyprctl_for(signature, &["-j", subcommand]).await?;
    serde_json::from_slice(&stdout)
        .with_context(|| format!("could not parse `hyprctl -j {subcommand}` output as JSON"))
}

/// Query clients, workspaces, and monitors in one consistent-enough snapshot.
///
/// The session is resolved once so all three queries describe the same
/// compositor. If the attached signature turns out to be stale between
/// resolution and the query, one retry targets a freshly discovered sole live
/// instance.
pub async fn snapshot() -> Result<Snapshot> {
    let signature = resolve_signature().await?;
    match snapshot_for(&signature).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => match retry_signature(&signature).await {
            Some(retry) => match snapshot_for(&retry).await {
                Ok(snapshot) => Ok(snapshot),
                Err(_) => Err(error),
            },
            None => Err(error),
        },
    }
}

async fn snapshot_for(signature: &str) -> Result<Snapshot> {
    let (clients, workspaces, monitors) = tokio::try_join!(
        query::<Vec<Client>>(signature, "clients"),
        query::<Vec<Workspace>>(signature, "workspaces"),
        query::<Vec<Monitor>>(signature, "monitors"),
    )?;
    Ok(Snapshot {
        clients,
        workspaces,
        monitors,
    })
}

/// Current global cursor position.
///
/// Newer Hyprland versions support `cursorpos -j`; older ones print
/// `x, y`, so both are accepted.
pub async fn cursor_position() -> Result<(i32, i32)> {
    let signature = resolve_signature().await?;
    match cursor_position_for(&signature).await {
        Ok(position) => Ok(position),
        Err(error) => match retry_signature(&signature).await {
            Some(retry) => match cursor_position_for(&retry).await {
                Ok(position) => Ok(position),
                Err(_) => Err(error),
            },
            None => Err(error),
        },
    }
}

async fn cursor_position_for(signature: &str) -> Result<(i32, i32)> {
    match query::<CursorPos>(signature, "cursorpos").await {
        Ok(cursor) => Ok((cursor.x, cursor.y)),
        Err(json_error) => {
            let stdout = hyprctl_for(signature, &["cursorpos"])
                .await
                .map_err(|text_error| {
                    anyhow::anyhow!("could not read cursor position ({json_error}; {text_error})")
                })?;
            let text = String::from_utf8_lossy(&stdout);
            parse_cursor_position(&text).ok_or_else(|| {
                anyhow::anyhow!("unexpected `hyprctl cursorpos` output: {}", text.trim())
            })
        }
    }
}

/// Parse the legacy `hyprctl cursorpos` text form (`"1920, 1080"`).
pub fn parse_cursor_position(text: &str) -> Option<(i32, i32)> {
    let (x, y) = text.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Subscribe to Hyprland socket2 and signal whenever an anchor-relevant event
/// arrives. The receiver calls [`snapshot`] again to obtain exact state.
///
/// The subscription is resilient to compositor restarts: if the socket is
/// missing, closes, or fails, it re-resolves the instance and reconnects with
/// a bounded exponential backoff. Every successful connection emits a refresh
/// signal, including the first one, so anchors converge when the daemon
/// started before the compositor, and events missed while disconnected are
/// not lost. Cancellation ends the subscription cleanly; a dropped receiver
/// also ends it quietly.
pub async fn subscribe(events: UnboundedSender<()>, cancel: CancellationToken) -> Result<()> {
    subscribe_with(events, cancel, resolve_event_socket_async).await
}

/// Reconnecting subscribe loop with an injectable socket resolver.
async fn subscribe_with<F, Fut>(
    events: UnboundedSender<()>,
    cancel: CancellationToken,
    mut resolve: F,
) -> Result<()>
where
    F: FnMut(bool) -> Fut,
    Fut: Future<Output = Result<PathBuf, AnchorError>>,
{
    let mut backoff = RECONNECT_MIN;
    let mut prefer_discovery = false;

    loop {
        if cancel.is_cancelled() || events.is_closed() {
            return Ok(());
        }

        // Resolve on every attempt so a restart with a new signature is
        // picked up without restarting the daemon. After a failed connect,
        // prefer a freshly discovered sole live instance over the inherited
        // signature whose socket just failed.
        let path = match resolve(prefer_discovery).await {
            Ok(path) => path,
            Err(_) => {
                prefer_discovery = false;
                if !wait_or_cancel(&cancel, backoff).await {
                    return Ok(());
                }
                backoff = next_backoff(backoff);
                continue;
            }
        };

        let connected = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            connected = UnixStream::connect(&path) => connected,
        };
        let stream = match connected {
            Ok(stream) => {
                prefer_discovery = false;
                stream
            }
            Err(_) => {
                prefer_discovery = true;
                if !wait_or_cancel(&cancel, backoff).await {
                    return Ok(());
                }
                backoff = next_backoff(backoff);
                continue;
            }
        };

        // Socket events are not replayed. Signal a refresh on every
        // successful connection so a daemon that started before the
        // compositor, or one that missed events while disconnected, converges.
        if events.send(()).is_err() {
            // The daemon dropped the receiver; stop quietly.
            return Ok(());
        }

        let connected_at = Instant::now();
        let mut lines = BufReader::new(stream).lines();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if is_tracked_event(&line) && events.send(()).is_err() {
                                return Ok(());
                            }
                        }
                        // EOF or a read error means the compositor restarted
                        // or the socket was replaced. Reconnect instead of
                        // ending the subscription for good.
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }

        if connected_at.elapsed() >= STABLE_CONNECTION {
            backoff = RECONNECT_MIN;
        }
        if !wait_or_cancel(&cancel, backoff).await {
            return Ok(());
        }
        backoff = next_backoff(backoff);
    }
}

/// Sleep unless cancellation arrives first. Returns `false` when cancelled.
async fn wait_or_cancel(cancel: &CancellationToken, duration: Duration) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(RECONNECT_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_cursor_text() {
        assert_eq!(parse_cursor_position("1920, 1080\n"), Some((1920, 1080)));
        assert_eq!(parse_cursor_position("-10,40"), Some((-10, 40)));
        assert_eq!(parse_cursor_position("nonsense"), None);
        assert_eq!(parse_cursor_position("12, x"), None);
    }

    #[test]
    fn extracts_event_names() {
        assert_eq!(event_name("movewindow>>0x55d0,0,100"), "movewindow");
        assert_eq!(event_name("closewindow>>0x1001\n"), "closewindow");
        assert_eq!(event_name("workspace"), "workspace");
    }

    #[test]
    fn tracks_anchor_relevant_events_only() {
        assert!(is_tracked_event("openwindow>>0x1,1,kitty"));
        assert!(is_tracked_event("movewindowv2>>0x1,1,1920,0"));
        assert!(is_tracked_event("workspace>>2"));
        assert!(is_tracked_event("fullscreen>>1"));
        assert!(is_tracked_event("monitorremoved>>HDMI-A-1"));
        assert!(is_tracked_event("togglegroup>>0x1"));
        assert!(!is_tracked_event("submap>>default"));
        assert!(!is_tracked_event("keybind>>Super,Q"));
        assert!(!is_tracked_event(""));
    }

    #[test]
    fn socket_dirs_include_runtime_and_tmp() {
        let dirs = socket_dirs("abc", Some(Path::new("/run/user/1000")));
        assert_eq!(dirs[0], PathBuf::from("/run/user/1000/hypr/abc"));
        assert_eq!(dirs[1], PathBuf::from("/tmp/hypr/abc"));

        let dirs = socket_dirs("abc", None);
        assert_eq!(dirs, vec![PathBuf::from("/tmp/hypr/abc")]);
    }

    #[test]
    fn parses_instance_list_from_hyprctl() {
        let instances = parse_instances(
            br#"[
                {
                    "instance": "efb5_1792781390_1711523624",
                    "time": 1792781390,
                    "pid": 35967,
                    "wl_socket": "wayland-1"
                }
            ]"#,
        )
        .expect("instances JSON");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].signature(), Some("efb5_1792781390_1711523624"));
    }

    #[test]
    fn parses_instance_paths_and_wrapped_objects() {
        let instances = parse_instances(
            br#"{"instances": [
                {"instance": "/tmp/hypr/older-instance/"},
                {"signature": "explicit-signature"}
            ]}"#,
        )
        .expect("wrapped instances JSON");
        assert_eq!(instances[0].signature(), Some("older-instance"));
        assert_eq!(instances[1].signature(), Some("explicit-signature"));
        assert_eq!(parse_instances(b"not json"), None);
        assert_eq!(parse_instances(b"{}"), None);
    }

    #[test]
    fn attached_live_session_wins_over_other_instances() {
        let signature = select_live_signature(
            Some("attached"),
            &["attached".to_string(), "other".to_string()],
        )
        .expect("attached session");
        assert_eq!(signature, "attached");
    }

    #[test]
    fn stale_socket_file_is_ignored_when_a_new_instance_is_live() {
        // Regression: a crashed compositor can leave `.socket.sock` behind.
        // The authoritative live list must replace the stale inherited
        // signature with the sole live instance even though the old socket
        // file still exists.
        let signature = select_live_signature(Some("stale"), &["fresh".to_string()])
            .expect("recover to fresh instance");
        assert_eq!(signature, "fresh");
    }

    #[test]
    fn unset_signature_recovers_to_the_sole_live_instance() {
        let signature = select_live_signature(None, &["fresh".to_string()])
            .expect("discover the only instance");
        assert_eq!(signature, "fresh");
    }

    #[test]
    fn multiple_live_instances_with_a_stale_attachment_are_refused() {
        let error = select_live_signature(Some("stale"), &["one".to_string(), "two".to_string()])
            .expect_err("ambiguous session");
        assert!(matches!(error, AnchorError::DesktopUnavailable(_)));
        let message = error.to_string();
        assert!(message.contains("stale"), "{message}");
        assert!(message.contains("one"), "{message}");
        assert!(message.contains("two"), "{message}");
        assert!(message.contains("refusing to guess"), "{message}");
    }

    #[test]
    fn multiple_live_instances_without_an_attachment_are_refused() {
        let error = select_live_signature(None, &["one".to_string(), "two".to_string()])
            .expect_err("ambiguous session");
        assert!(matches!(error, AnchorError::DesktopUnavailable(_)));
        assert!(error.to_string().contains("HYPRLAND_INSTANCE_SIGNATURE"));
    }

    #[test]
    fn headless_session_is_reported_unavailable() {
        let error = select_live_signature(None, &[]).expect_err("headless");
        assert!(matches!(error, AnchorError::DesktopUnavailable(_)));
        assert!(error.to_string().contains("no live Hyprland instance"));

        let error = select_socket_signature(None, |_| false).expect_err("headless");
        assert!(matches!(error, AnchorError::DesktopUnavailable(_)));
        assert!(error.to_string().contains("HYPRLAND_INSTANCE_SIGNATURE"));

        let error = select_socket_signature(Some("stale-instance"), |_| false)
            .expect_err("missing live instance");
        assert!(matches!(error, AnchorError::DesktopUnavailable(_)));
        assert!(error.to_string().contains("stale-instance"));
    }

    #[test]
    fn attached_socket_is_only_a_fallback_without_a_live_list() {
        let signature = select_socket_signature(Some("live"), |signature| signature == "live")
            .expect("attached socket");
        assert_eq!(signature, "live");
        assert!(select_socket_signature(Some("live"), |_| false).is_err());
    }

    #[test]
    fn reconnect_backoff_grows_and_stays_bounded() {
        assert_eq!(RECONNECT_MIN, Duration::from_millis(200));
        let mut backoff = RECONNECT_MIN;
        for _ in 0..10 {
            let next = next_backoff(backoff);
            assert!(next >= backoff);
            assert!(next <= RECONNECT_MAX);
            backoff = next;
        }
        assert_eq!(backoff, RECONNECT_MAX);
    }

    #[tokio::test]
    async fn subscribe_reconnects_and_signals_a_refresh_after_socket_loss() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixListener;

        let dir = std::env::temp_dir().join(format!(
            "buddy-anchor-subscribe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(EVENT_SOCKET);

        let listener = UnixListener::bind(&path).expect("bind event socket");
        let (events, mut signals) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let resolver_path = path.clone();
        let handle = tokio::spawn(subscribe_with(
            events,
            cancel.clone(),
            move |_prefer_discovery| {
                let path = resolver_path.clone();
                async move { Ok::<_, AnchorError>(path) }
            },
        ));

        // Every successful connection asks for a refresh, including the
        // first one, so a daemon that started before the compositor converges.
        let (mut stream, _) = listener.accept().await.expect("first connection");
        tokio::time::timeout(Duration::from_secs(2), signals.recv())
            .await
            .expect("initial refresh")
            .expect("channel open");

        stream
            .write_all(b"workspace>>2\n")
            .await
            .expect("write event");
        stream.flush().await.expect("flush");
        tokio::time::timeout(Duration::from_secs(2), signals.recv())
            .await
            .expect("event signal")
            .expect("channel open");

        // Drop the listener and connection: the subscription must survive the
        // socket loss instead of ending for good.
        drop(stream);
        drop(listener);
        std::fs::remove_file(&path).expect("remove old socket");
        let listener = UnixListener::bind(&path).expect("rebind event socket");

        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("reconnect")
            .expect("second connection");

        // A successful reconnect asks the daemon for a fresh snapshot because
        // events during the gap were lost.
        tokio::time::timeout(Duration::from_secs(2), signals.recv())
            .await
            .expect("refresh after reconnect")
            .expect("channel open");

        stream
            .write_all(b"closewindow>>0x1001\n")
            .await
            .expect("write event after reconnect");
        stream.flush().await.expect("flush");
        tokio::time::timeout(Duration::from_secs(2), signals.recv())
            .await
            .expect("event after reconnect")
            .expect("channel open");

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("subscription exits on cancel")
            .expect("task join")
            .expect("subscription result");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
