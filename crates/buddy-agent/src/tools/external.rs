//! External stdio tools.
//!
//! Protocol (matches `examples/external-tool.py` and `examples/config.toml`):
//!
//! * The engine spawns `command` as an argv vector, never through a shell, with
//!   its working directory set to the confined project root.
//! * It writes one JSON line to stdin: `{"name":"<tool>","arguments":{...}}`.
//! * The tool writes one JSON object to stdout. `{"result":"..."}` is success;
//!   `{"error":"..."}` is failure. Plain text stdout is accepted as result text
//!   for simple scripts.
//! * Stderr is diagnostic only and is not sent to the model.
//!
//! Each call is a fresh process, so no call id is needed on the wire. A non-zero
//! exit status always becomes an error result even if the tool printed a result.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::tools::process;
use crate::tools::{ToolContext, ToolOutcome};
use crate::ExternalTool;

const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn execute(
    ctx: &ToolContext,
    tool: &ExternalTool,
    arguments: &Value,
    cancel: &CancellationToken,
) -> ToolOutcome {
    if tool.command.is_empty() || tool.command[0].trim().is_empty() {
        return ToolOutcome::error(format!(
            "external tool `{}` has no executable command configured.",
            tool.name
        ));
    }

    let arguments = if arguments.is_null() {
        json!({})
    } else if arguments.is_object() {
        arguments.clone()
    } else {
        return ToolOutcome::error(format!(
            "external tool `{}` requires a JSON object of arguments.",
            tool.name
        ));
    };

    let request = json!({ "name": tool.name, "arguments": arguments });
    let mut line = match serde_json::to_vec(&request) {
        Ok(line) => line,
        Err(err) => {
            return ToolOutcome::error(format!(
                "external tool `{}` arguments could not be serialized: {err}",
                tool.name
            ))
        }
    };
    line.push(b'\n');

    if cancel.is_cancelled() {
        return ToolOutcome::error(format!(
            "external tool `{}` was cancelled before it started.",
            tool.name
        ));
    }

    let mut command = Command::new(&tool.command[0]);
    command
        .args(&tool.command[1..])
        .current_dir(&ctx.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    process::isolated(&mut command);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ToolOutcome::error(format!(
                "external tool `{}` failed to start: {err}",
                tool.name
            ))
        }
    };

    let output = process::run(child, Some(line), EXTERNAL_TIMEOUT, cancel).await;
    if output.cancelled {
        return ToolOutcome::error(format!(
            "external tool `{}` was cancelled and its process was killed.",
            tool.name
        ));
    }
    if output.timed_out {
        return ToolOutcome::error(format!(
            "external tool `{}` timed out after {}s.",
            tool.name,
            EXTERNAL_TIMEOUT.as_secs()
        ));
    }
    if let Some(error) = output.error {
        return ToolOutcome::error(format!("external tool `{}`: {error}", tool.name));
    }

    let success = output
        .status
        .map(|status| status.success())
        .unwrap_or(false);
    let code = output.status.and_then(|status| status.code());
    parse_external_response(&output.stdout, success, code)
}

/// Interpret a tool's stdout. `stderr` is intentionally absent: diagnostics are
/// never sent to the model.
fn parse_external_response(stdout: &str, success: bool, code: Option<i32>) -> ToolOutcome {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return if success {
            ToolOutcome::ok("(no output)")
        } else {
            ToolOutcome::error(exit_message("external tool produced no output", code))
        };
    }

    let payload = parse_payload(trimmed);
    let (content, declared_error) = match payload {
        Some(Value::String(text)) => (text, false),
        Some(value @ Value::Object(_)) => interpret_object(&value),
        Some(other) => (other.to_string(), false),
        None => (trimmed.to_string(), false),
    };

    if !success {
        let message = if declared_error || !content.is_empty() {
            exit_message(&content, code)
        } else {
            exit_message("external tool failed", code)
        };
        return ToolOutcome::error(message);
    }

    if declared_error {
        return ToolOutcome::error(content);
    }
    ToolOutcome::ok(content)
}

fn interpret_object(value: &Value) -> (String, bool) {
    let object = value.as_object().expect("object");

    if let Some(error) = object.get("error") {
        let message = render_result(error);
        return (
            if message.trim().is_empty() {
                "external tool reported an error".to_string()
            } else {
                message
            },
            true,
        );
    }

    if let Some(result) = object.get("result") {
        return (render_result(result), false);
    }

    if let Some(content) = object.get("content") {
        return (render_result(content), false);
    }

    if let Some(text) = object.get("text") {
        return (render_result(text), false);
    }

    let is_error = object
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_error {
        let message = object
            .get("message")
            .map(render_result)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "external tool reported an error".to_string());
        return (message, true);
    }

    (
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        false,
    )
}

fn parse_payload(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    // Tolerate scripts that print logging lines before the final JSON object.
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if value.is_object() || value.is_string() {
                return Some(value);
            }
        }
    }
    None
}

fn render_result(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(render_block)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => {
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(content) = object.get("content") {
                return render_result(content);
            }
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        other => other.to_string(),
    }
}

fn render_block(block: &Value) -> String {
    match block {
        Value::String(text) => text.clone(),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("image") {
                return "[image omitted]".to_string();
            }
            match object.get("text").and_then(Value::as_str) {
                Some(text) => text.to_string(),
                None => serde_json::to_string(block).unwrap_or_default(),
            }
        }
        other => other.to_string(),
    }
}

fn exit_message(content: &str, code: Option<i32>) -> String {
    let exit = code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none (terminated by signal)".to_string());
    if content.trim().is_empty() {
        format!("external tool failed (exit_code: {exit}).")
    } else {
        format!("{content}\n\nexit_code: {exit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;
    use crate::testutil::TempDir;

    fn context() -> (TempDir, ToolContext) {
        let temp = TempDir::new();
        let root = paths::canonical_root(temp.path()).unwrap();
        let ctx = ToolContext {
            root,
            allow_shell: true,
            skills: Vec::new(),
            writes: Default::default(),
        };
        (temp, ctx)
    }

    fn external(command: Vec<&str>) -> ExternalTool {
        ExternalTool {
            name: "echo".into(),
            description: "test tool".into(),
            command: command.into_iter().map(str::to_string).collect(),
            parameters: json!({"type":"object","properties":{"text":{"type":"string"}}}),
            risk: "read-only".into(),
        }
    }

    #[test]
    fn parses_result_and_error_shapes() {
        let ok = parse_external_response(r#"{"result":"pong"}"#, true, Some(0));
        assert!(!ok.is_error);
        assert_eq!(ok.content, "pong");

        let err = parse_external_response(r#"{"error":"bad input"}"#, false, Some(1));
        assert!(err.is_error);
        assert!(err.content.starts_with("bad input"));

        let plain = parse_external_response("just text", true, Some(0));
        assert!(!plain.is_error);
        assert_eq!(plain.content, "just text");

        let failed = parse_external_response("partial", false, Some(2));
        assert!(failed.is_error);
        assert!(failed.content.contains("exit_code: 2"));
    }

    #[test]
    fn parses_content_blocks_and_tolerates_log_lines() {
        let blocks = parse_external_response(
            r#"{"content":[{"type":"text","text":"a"},{"type":"image","data":"x"}]}"#,
            true,
            Some(0),
        );
        assert_eq!(blocks.content, "a\n[image omitted]");

        let logged = parse_external_response("starting...\n{\"result\":\"done\"}\n", true, Some(0));
        assert_eq!(logged.content, "done");
    }

    #[test]
    fn non_zero_exit_overrides_a_result() {
        let outcome = parse_external_response(r#"{"result":"looks fine"}"#, false, Some(1));
        assert!(outcome.is_error);
        assert!(outcome.content.contains("looks fine"));
        assert!(outcome.content.contains("exit_code: 1"));
    }

    #[tokio::test]
    async fn writes_name_and_arguments_and_reads_result() {
        let (_temp, ctx) = context();
        let tool = external(vec![
            "/bin/sh",
            "-c",
            r#"cat > request.json; printf '%s' '{"result":"pong"}'"#,
        ]);
        let token = CancellationToken::new();
        let outcome = execute(&ctx, &tool, &json!({"text":"hi"}), &token).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(outcome.content, "pong");

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(ctx.root.join("request.json")).unwrap())
                .unwrap();
        assert_eq!(request["name"], "echo");
        assert_eq!(request["arguments"]["text"], "hi");
        assert!(request.get("call_id").is_none());
    }

    #[tokio::test]
    async fn missing_command_is_an_error() {
        let (_temp, ctx) = context();
        let tool = external(Vec::new());
        let token = CancellationToken::new();
        let outcome = execute(&ctx, &tool, &json!({}), &token).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("no executable command"));
    }

    #[tokio::test]
    async fn non_object_arguments_are_rejected() {
        let (_temp, ctx) = context();
        let tool = external(vec!["/bin/true"]);
        let token = CancellationToken::new();
        let outcome = execute(&ctx, &tool, &json!("nope"), &token).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("JSON object"));
    }
}
