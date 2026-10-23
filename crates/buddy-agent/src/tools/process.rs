//! Shared child-process execution with cancellation, timeout, bounded output
//! capture, and process-group termination.
//!
//! Callers must call [`isolated`] before spawning so the child becomes its own
//! process-group leader. Cancellation then sends `SIGKILL` to the whole group
//! (`-pgid`), which stops shell descendants as well as the immediate child. A
//! drop guard provides the same guarantee if the engine future is dropped
//! before it can observe cancellation.

use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const STREAM_CAP_BYTES: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const KILL_WAIT: Duration = Duration::from_secs(5);
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// Put the child in a fresh process group so cancellation can kill descendants.
pub(crate) fn isolated(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

pub(crate) struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<ExitStatus>,
    pub cancelled: bool,
    pub timed_out: bool,
    pub error: Option<String>,
}

/// Run a spawned child (spawned after [`isolated`]), feeding `stdin_bytes` when
/// provided.
///
/// Cancellation and timeout send `SIGKILL` to the child's process group and
/// return promptly; this is the mechanism the global kill switch relies on.
pub(crate) async fn run(
    mut child: Child,
    stdin_bytes: Option<Vec<u8>>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> ProcessOutput {
    let mut group = GroupKill::new(&child);
    let stdout_task = child.stdout.take().map(spawn_reader::<ChildStdout>);
    let stderr_task = child.stderr.take().map(spawn_reader::<ChildStderr>);

    if let Some(mut stdin) = child.stdin.take() {
        let bytes = stdin_bytes.unwrap_or_default();
        tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        });
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let mut cancelled = false;
    let mut timed_out = false;
    let mut error = None;
    let mut status = None;

    loop {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {}
            Err(err) => {
                error = Some(format!("failed to wait for process: {err}"));
                break;
            }
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
        }
        let nap = (deadline - now).min(POLL_INTERVAL);
        tokio::select! {
            _ = cancel.cancelled() => {}
            _ = tokio::time::sleep(nap) => {}
        }
    }

    if cancelled || timed_out || error.is_some() {
        group.kill_tree(&mut child);
        let _ = tokio::time::timeout(KILL_WAIT, child.wait()).await;
        group.disarm();
    } else {
        // Natural completion: leave intentionally backgrounded descendants
        // alone. Only cancellation/timeout is a kill-switch event.
        group.disarm();
    }

    ProcessOutput {
        stdout: finish(stdout_task).await,
        stderr: finish(stderr_task).await,
        status,
        cancelled,
        timed_out,
        error,
    }
}

struct GroupKill {
    pgid: Option<i32>,
    armed: bool,
}

impl GroupKill {
    fn new(child: &Child) -> Self {
        Self {
            pgid: child.id().map(|id| id as i32),
            armed: true,
        }
    }

    /// Kill the whole process group first, then the direct child as a fallback
    /// in case the process-group setup failed for some reason.
    fn kill_tree(&mut self, child: &mut Child) {
        if let Some(pgid) = self.pgid {
            kill_group(pgid);
        }
        let _ = child.start_kill();
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        if self.armed {
            if let Some(pgid) = self.pgid {
                kill_group(pgid);
            }
        }
    }
}

#[cfg(unix)]
fn kill_group(pgid: i32) {
    if pgid > 0 {
        // Negative pid means "the process group whose id is |pid|".
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pgid: i32) {}

fn spawn_reader<R>(mut stream: R) -> JoinHandle<String>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut collected = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut truncated = false;
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => {
                    if collected.len() < STREAM_CAP_BYTES {
                        let room = STREAM_CAP_BYTES - collected.len();
                        let take = room.min(read);
                        collected.extend_from_slice(&chunk[..take]);
                        if take < read {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(_) => break,
            }
        }
        let mut text = String::from_utf8_lossy(&collected).into_owned();
        if truncated {
            text.push_str("\n...[stream truncated]");
        }
        text
    })
}

async fn finish(task: Option<JoinHandle<String>>) -> String {
    let Some(mut task) = task else {
        return String::new();
    };
    match tokio::time::timeout(DRAIN_WAIT, &mut task).await {
        Ok(Ok(text)) => text,
        _ => {
            task.abort();
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn child(script: &str) -> Child {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        isolated(&mut command);
        command.spawn().unwrap()
    }

    #[tokio::test]
    async fn captures_output_and_status() {
        let token = CancellationToken::new();
        let output = run(
            child("printf out; printf err >&2; exit 3"),
            None,
            Duration::from_secs(5),
            &token,
        )
        .await;
        assert_eq!(output.stdout, "out");
        assert_eq!(output.stderr, "err");
        assert_eq!(output.status.unwrap().code(), Some(3));
        assert!(!output.timed_out);
        assert!(!output.cancelled);
    }

    #[tokio::test]
    async fn timeout_kills_the_child() {
        let token = CancellationToken::new();
        let started = std::time::Instant::now();
        let output = run(child("sleep 30"), None, Duration::from_millis(150), &token).await;
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancellation_kills_the_child() {
        let token = CancellationToken::new();
        let started = std::time::Instant::now();
        let task = {
            let token = token.clone();
            tokio::spawn(async move {
                run(child("sleep 30"), None, Duration::from_secs(60), &token).await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
        let output = task.await.unwrap();
        assert!(output.cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancellation_kills_descendants_in_the_group() {
        let token = CancellationToken::new();
        let dir = std::env::temp_dir().join(format!("buddy-proc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = format!(
            "(sleep 1; echo leaked > {}/leaked.txt) & sleep 30",
            dir.display()
        );

        let task = {
            let token = token.clone();
            tokio::spawn(
                async move { run(child(&script), None, Duration::from_secs(60), &token).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        token.cancel();
        let output = task.await.unwrap();
        assert!(output.cancelled);

        tokio::time::sleep(Duration::from_millis(1200)).await;
        let leaked = dir.join("leaked.txt").exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!leaked, "descendant shell survived process-group kill");
    }

    #[tokio::test]
    async fn sends_stdin() {
        let token = CancellationToken::new();
        let output = run(
            child("cat"),
            Some(b"hello".to_vec()),
            Duration::from_secs(5),
            &token,
        )
        .await;
        assert_eq!(output.stdout, "hello");
    }
}
