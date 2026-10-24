//! Bash tool: a dangerous tool that is present only when `allow_shell` is true
//! and always needs one approval per invocation before it gets here.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::tools::process;
use crate::tools::{optional_u64, string_arg, ToolContext, ToolOutcome};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_COMMAND_BYTES: usize = 20_000;

pub(crate) async fn bash(
    ctx: &ToolContext,
    arguments: &Value,
    cancel: &CancellationToken,
) -> ToolOutcome {
    if !ctx.allow_shell {
        return ToolOutcome::error(
            "bash is disabled for this session (allow_shell is false). Do not attempt shell access.",
        );
    }

    let command_line = match string_arg(arguments, "command") {
        Ok(command) => command,
        Err(outcome) => return outcome,
    };
    if command_line.trim().is_empty() {
        return ToolOutcome::error("bash: `command` must not be empty.");
    }
    if command_line.len() > MAX_COMMAND_BYTES {
        return ToolOutcome::error(format!(
            "bash: command is longer than the {MAX_COMMAND_BYTES} byte limit."
        ));
    }

    // `timeout` is the Pi spelling; `timeout_seconds` is what our schema advertises.
    let timeout_secs = optional_u64(arguments, "timeout_seconds")
        .or_else(|| optional_u64(arguments, "timeout"))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);

    if cancel.is_cancelled() {
        return ToolOutcome::error("bash command cancelled before it started.");
    }

    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(command_line)
        .current_dir(&ctx.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The child must die if the engine future is dropped unexpectedly.
        .kill_on_drop(true);
    // Do not let a parent-supplied BASH_ENV inject startup code into the tool.
    command.env_remove("BASH_ENV");
    // Its own process group, so cancellation kills `sleep`, pipelines, etc.
    process::isolated(&mut command);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ToolOutcome::error(format!(
                "bash: failed to start process in `{}`: {err}",
                ctx.root.display()
            ))
        }
    };

    let output = process::run(child, None, Duration::from_secs(timeout_secs), cancel).await;

    if output.cancelled {
        return ToolOutcome::error("bash command cancelled; the running process was killed.");
    }
    if output.timed_out {
        let mut message =
            format!("bash command timed out after {timeout_secs}s; the process was killed.");
        append_partial(&mut message, &output.stdout, &output.stderr);
        return ToolOutcome::error(message);
    }
    if let Some(error) = output.error {
        return ToolOutcome::error(format!("bash: {error}"));
    }

    let success = output
        .status
        .map(|status| status.success())
        .unwrap_or(false);
    let exit = output
        .status
        .and_then(|status| status.code())
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none (terminated by signal)".to_string());

    let mut parts: Vec<String> = Vec::new();
    if !output.stdout.trim_end().is_empty() {
        parts.push(format!("stdout:\n{}", output.stdout.trim_end()));
    }
    if !output.stderr.trim_end().is_empty() {
        parts.push(format!("stderr:\n{}", output.stderr.trim_end()));
    }
    if parts.is_empty() {
        parts.push("(no output)".to_string());
    }
    parts.push(format!("exit_code: {exit}"));
    let content = parts.join("\n\n");

    if success {
        ToolOutcome::ok(content)
    } else {
        ToolOutcome::error(content)
    }
}

fn append_partial(message: &mut String, stdout: &str, stderr: &str) {
    if !stdout.trim_end().is_empty() {
        message.push_str(&format!("\n\nstdout:\n{}", stdout.trim_end()));
    }
    if !stderr.trim_end().is_empty() {
        message.push_str(&format!("\n\nstderr:\n{}", stderr.trim_end()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;
    use crate::testutil::TempDir;

    fn context(allow_shell: bool) -> (TempDir, ToolContext) {
        let temp = TempDir::new();
        let root = paths::canonical_root(temp.path()).unwrap();
        let ctx = ToolContext {
            root,
            allow_shell,
            skills: Vec::new(),
            writes: Default::default(),
        };
        (temp, ctx)
    }

    #[tokio::test]
    async fn refuses_to_run_without_allow_shell() {
        let (_temp, ctx) = context(false);
        let token = CancellationToken::new();
        let outcome = bash(&ctx, &serde_json::json!({"command":"echo hi"}), &token).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("disabled"));
    }

    #[tokio::test]
    async fn runs_in_project_root_and_reports_exit_code() {
        let (_temp, ctx) = context(true);
        std::fs::write(ctx.root.join("marker.txt"), "x").unwrap();
        let token = CancellationToken::new();
        let outcome = bash(
            &ctx,
            &serde_json::json!({"command":"pwd; printf err >&2; exit 3"}),
            &token,
        )
        .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains(&ctx.root.display().to_string()));
        assert!(outcome.content.contains("stderr:\nerr"));
        assert!(outcome.content.contains("exit_code: 3"));
    }

    #[tokio::test]
    async fn success_is_not_an_error() {
        let (_temp, ctx) = context(true);
        let token = CancellationToken::new();
        let outcome = bash(&ctx, &serde_json::json!({"command":"printf hello"}), &token).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert!(outcome.content.contains("stdout:\nhello"));
        assert!(outcome.content.contains("exit_code: 0"));
    }

    #[tokio::test]
    async fn cancellation_kills_the_shell() {
        let (_temp, ctx) = context(true);
        let token = CancellationToken::new();
        let task = {
            let token = token.clone();
            let ctx_root = ctx.root.clone();
            tokio::spawn(async move {
                let ctx = ToolContext {
                    root: ctx_root,
                    allow_shell: true,
                    skills: Vec::new(),
                    writes: Default::default(),
                };
                bash(
                    &ctx,
                    &serde_json::json!({"command":"echo $$ > pid.txt; sleep 30"}),
                    &token,
                )
                .await
            })
        };

        let pid_path = ctx.root.join("pid.txt");
        for _ in 0..100 {
            if pid_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = std::fs::read_to_string(&pid_path).unwrap_or_default();
        let pid: u32 = pid.trim().parse().unwrap_or(0);
        assert!(pid > 0, "shell pid was not written");

        token.cancel();
        let outcome = task.await.unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("cancelled"));

        tokio::time::sleep(Duration::from_millis(100)).await;
        let alive = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!alive, "bash child {pid} survived cancellation");
    }

    #[tokio::test]
    async fn cancellation_kills_forked_descendants_before_they_write() {
        let (_temp, ctx) = context(true);
        let token = CancellationToken::new();
        let task = {
            let token = token.clone();
            let ctx_root = ctx.root.clone();
            tokio::spawn(async move {
                let ctx = ToolContext {
                    root: ctx_root,
                    allow_shell: true,
                    skills: Vec::new(),
                    writes: Default::default(),
                };
                bash(
                    &ctx,
                    &serde_json::json!({
                        "command": "(sleep 1; echo leaked > leaked.txt) & echo $$ > pid.txt; sleep 30"
                    }),
                    &token,
                )
                .await
            })
        };

        let pid_path = ctx.root.join("pid.txt");
        for _ in 0..100 {
            if pid_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(pid_path.exists(), "shell did not start");

        token.cancel();
        let outcome = task.await.unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("cancelled"));

        // The forked subshell had a one-second delay. If it survived the group
        // kill it would create leaked.txt.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(
            !ctx.root.join("leaked.txt").exists(),
            "forked descendant survived cancellation and wrote a file"
        );
    }

    #[tokio::test]
    async fn timeout_is_reported() {
        let (_temp, ctx) = context(true);
        let token = CancellationToken::new();
        let started = std::time::Instant::now();
        let outcome = bash(
            &ctx,
            &serde_json::json!({"command":"sleep 30","timeout_seconds":1}),
            &token,
        )
        .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
