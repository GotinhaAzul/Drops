//! Test-only helpers.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// A unique temporary directory removed on drop.
///
/// The crate has no `tempfile` dependency; this is enough for unit tests and
/// avoids adding a dev-dependency to the shared workspace.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("buddy-agent-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Panic unless `messages` is valid strict OpenAI chat-completions wire data.
///
/// Strict providers reject: a `tool` message without an immediately preceding
/// assistant batch that names it, a `tool_calls` id with no result, duplicate
/// results, duplicate call ids, and calls without an id or name.
pub(crate) fn assert_strict_wire(messages: &[Value]) {
    let errors = strict_wire_errors(messages);
    assert!(
        errors.is_empty(),
        "invalid wire messages: {errors:?}\n{messages:#?}"
    );
}

pub(crate) fn strict_wire_errors(messages: &[Value]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let role = messages[index]
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("");
        match role {
            "system" | "user" => index += 1,
            "assistant" => {
                let Some(calls) = messages[index].get("tool_calls").and_then(Value::as_array)
                else {
                    index += 1;
                    continue;
                };

                let mut ids: Vec<String> = Vec::new();
                for call in calls {
                    let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                    let name = call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if id.is_empty() || name.trim().is_empty() {
                        errors.push(format!(
                            "assistant tool call at {index} has an empty id or name"
                        ));
                    }
                    if ids.iter().any(|seen| seen == id) {
                        errors.push(format!("duplicate tool call id {id:?} at {index}"));
                    }
                    ids.push(id.to_string());
                }

                let mut answered: Vec<String> = Vec::new();
                let mut next = index + 1;
                while next < messages.len()
                    && messages[next].get("role").and_then(Value::as_str) == Some("tool")
                {
                    let id = messages[next]
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !ids.iter().any(|call| call == id) {
                        errors.push(format!(
                            "tool message at {next} answers unknown call id {id:?}"
                        ));
                    }
                    if answered.iter().any(|seen| seen == id) {
                        errors.push(format!("duplicate tool result for call id {id:?}"));
                    }
                    answered.push(id.to_string());
                    next += 1;
                }
                for id in &ids {
                    if !answered.iter().any(|seen| seen == id) {
                        errors.push(format!("tool call {id:?} has no result"));
                    }
                }
                index = next;
            }
            "tool" => {
                errors.push(format!("orphan tool message at {index}"));
                index += 1;
            }
            other => {
                errors.push(format!("unexpected role {other:?} at {index}"));
                index += 1;
            }
        }
    }
    errors
}
