//! Rebuild UI history from the authoritative session log.
//!
//! The daemon persists `history`/`context` in `blobs.json` *after* forwarding
//! agent events, while [`buddy_agent`] appends every message to
//! `<session_dir>/<id>.jsonl` as it happens. A crash between those writes leaves
//! `blobs.json` behind the log: the UI shows a turn the daemon never recorded,
//! or misses a result whose tool call was already logged.
//!
//! [`hydrate`] replays the JSONL log into the same event shape the daemon
//! streams to the UI, so startup converges on the log as the source of truth.
//! The log is append-only, so a crash can also leave one malformed/truncated
//! final line; such lines are skipped rather than failing the whole session.

use anyhow::{Context as _, Result};
use serde_json::{json, Map, Value};

use buddy_proto::Blob;

/// Keep the same bound the daemon applies to live UI history.
const MAX_UI_ENTRIES: usize = 300;

/// Prefix the engine writes for the per-request context bundle. Those messages
/// are model input, not chat, so they never become visible history entries.
const CONTEXT_PREFIX: &str = "Context for this request (JSON):\n";

/// Rebuild `blob.history` (and `blob.context`) from `session_dir`.
///
/// Placement, id, state and approval are left untouched. A missing session file
/// means the blob has never run (or was cleared), so its existing history is
/// kept; a fresh idle blob therefore hydrates to itself.
///
/// Returns an error only for failures that are not crash artifacts: an invalid
/// blob id (path traversal) or an unreadable session file. Malformed JSON,
/// invalid UTF-8 and truncated lines are skipped.
pub fn hydrate(blob: &mut Blob, session_dir: &std::path::Path) -> Result<()> {
    let path = buddy_agent::session_path(session_dir, &blob.id)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("reading session {}", path.display())),
    };

    let mut entries: Vec<Value> = Vec::new();
    let mut tool_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut latest_context: Option<Value> = None;

    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        // Truncated or interleaved writes are expected after a crash; one bad
        // line must not hide the rest of the session.
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(message) = message_of(&value) else {
            continue;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let Some(text) = text_of(message.get("content")) else {
                    continue;
                };
                if let Some(rendered) = text.strip_prefix(CONTEXT_PREFIX) {
                    if let Ok(mut context) = serde_json::from_str::<Value>(rendered) {
                        if let Some(parts) = message["content"].as_array() {
                            if let Some(url) = parts.iter().find_map(|part| {
                                part.pointer("/image_url/url").and_then(Value::as_str)
                            }) {
                                if let Some(image) = context
                                    .pointer_mut("/content/image")
                                    .and_then(Value::as_object_mut)
                                {
                                    image.insert("data_url".into(), json!(url));
                                }
                            }
                        }
                        latest_context = Some(context);
                    }
                    continue;
                }
                if text.trim().is_empty() {
                    continue;
                }
                entries.push(json!({ "type": "user", "text": text }));
            }
            Some("assistant") => {
                if let Some(thinking) = thinking_of(message) {
                    entries.push(json!({ "type": "thinking", "text": thinking }));
                }
                if let Some(text) = text_of(message.get("content")) {
                    if !text.trim().is_empty() {
                        entries.push(json!({ "type": "text", "text": text }));
                    }
                }
                // Assistant messages carry the tool-call ids; tool results that
                // follow only reference them.
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let Some(id) = call.get("id").and_then(Value::as_str) else {
                            continue;
                        };
                        let name = call
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            .filter(|name| !name.is_empty());
                        if let Some(name) = name {
                            tool_names.insert(id.to_string(), name.to_string());
                        }
                    }
                }
            }
            Some("tool") => {
                let Some(text) = text_of(message.get("content")) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                let mut event = Map::new();
                event.insert("type".into(), json!("tool_result"));
                event.insert("text".into(), json!(text));
                let name = message
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .or_else(|| {
                        message
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .and_then(|id| tool_names.get(id).map(String::as_str))
                    });
                if let Some(name) = name {
                    event.insert("name".into(), json!(name));
                }
                entries.push(Value::Object(event));
            }
            // Session headers (`type: session`), system messages and unknown
            // art tags are not part of the visible chat.
            _ => {}
        }
    }

    if entries.len() > MAX_UI_ENTRIES {
        entries.drain(..entries.len() - MAX_UI_ENTRIES);
    }
    blob.history = entries;
    if let Some(context) = latest_context {
        blob.context = context;
    }
    Ok(())
}

/// The message object of a log line.
///
/// Current logs wrap messages as `{"type":"message","message":{...}}`; older
/// logs store the bare message. Header lines have no `role` and are ignored.
fn message_of(value: &Value) -> Option<&Value> {
    let message = value.get("message").unwrap_or(value);
    message.get("role")?.as_str()?;
    Some(message)
}

/// Decode message content, which may be a string, an array of text parts, or
/// another JSON value.
fn text_of(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(fragment) = part
                    .as_str()
                    .or_else(|| part.get("text").and_then(Value::as_str))
                {
                    text.push_str(fragment);
                }
            }
            Some(text)
        }
        other => Some(other.to_string()),
    }
}

/// Reasoning text when the log kept any. Streamed reasoning is stored with the
/// assistant message; older logs and providers that do not report it stay
/// readable because the field is optional.
fn thinking_of(message: &Value) -> Option<String> {
    ["reasoning_content", "reasoning", "thinking"]
        .iter()
        .find_map(|field| text_of(message.get(*field)))
        .filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Unique temporary directory removed on drop (no tempfile dependency).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("buddyd-recovery-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn blob(id: &str) -> Blob {
        Blob {
            appearance: buddy_proto::DropAppearance::default(),
            offset: [0, 0],
            id: id.to_string(),
            placement: json!({
                "anchor": { "kind": "workspace", "id": 1 },
                "x": 40,
                "y": 12,
                "visible": true,
                "label": "Headless workspace"
            }),
            state: "idle".into(),
            history: vec![json!({ "type": "text", "text": "stale" })],
            context: json!({ "stale": true }),
            approval: None,
        }
    }

    fn write_lines(dir: &Path, id: &str, lines: &[Value]) -> PathBuf {
        let path = dir.join(format!("{id}.jsonl"));
        let mut text = String::new();
        for line in lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    fn message(value: Value) -> Value {
        json!({ "type": "message", "timestamp": 1, "message": value })
    }

    #[test]
    fn replays_log_into_events_and_latest_context() {
        let temp = TempDir::new();
        write_lines(
            temp.path(),
            "s1",
            &[
                json!({ "type": "session", "version": 1, "id": "s1", "cwd": "/project" }),
                message(json!({
                    "role": "user",
                    "content": "Context for this request (JSON):\n{\"window\":{\"class\":\"kitty\"}}"
                })),
                message(json!({ "role": "user", "content": "hello" })),
                message(json!({
                    "role": "assistant",
                    "content": "checking",
                    "reasoning_content": "look first",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "read", "arguments": "{}" }
                    }]
                })),
                message(json!({ "role": "tool", "tool_call_id": "c1", "content": "file body" })),
                message(json!({ "role": "tool", "tool_call_id": "gone", "content": "orphan" })),
                message(json!({ "role": "assistant", "content": "done" })),
                message(json!({
                    "role": "user",
                    "content": "Context for this request (JSON):\n{\"window\":{\"class\":\"foot\"}}"
                })),
            ],
        );

        let mut b = blob("s1");
        let placement = b.placement.clone();
        hydrate(&mut b, temp.path()).unwrap();

        assert_eq!(
            b.history,
            vec![
                json!({ "type": "user", "text": "hello" }),
                json!({ "type": "thinking", "text": "look first" }),
                json!({ "type": "text", "text": "checking" }),
                json!({ "type": "tool_result", "text": "file body", "name": "read" }),
                json!({ "type": "tool_result", "text": "orphan" }),
                json!({ "type": "text", "text": "done" }),
            ]
        );
        assert!(b.history[4].get("name").is_none());
        assert_eq!(b.context, json!({ "window": { "class": "foot" } }));
        assert_eq!(b.placement, placement);
        assert_eq!(b.id, "s1");
        assert_eq!(b.state, "idle");
    }

    #[test]
    fn malformed_and_truncated_lines_are_skipped() {
        let temp = TempDir::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            format!("{}\n", json!({ "role": "user", "content": "kept" })).as_bytes(),
        );
        bytes.extend_from_slice(b"not json\n");
        bytes.extend_from_slice(
            b"{\"type\":\"message\",\"timestamp\":2,\"message\":{\"role\":\"user\",\"content\":\"partial",
        );
        bytes.extend_from_slice(b"\n\xff\xfe\n");
        bytes.extend_from_slice(
            format!(
                "{}\n",
                message(json!({
                    "role": "user",
                    "content": "Context for this request (JSON):\n{\"window\":"
                }))
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(
            format!("{}\n", json!({ "role": "user", "content": "kept2" })).as_bytes(),
        );
        bytes.extend_from_slice(
            format!("{}\n", json!({ "role": "assistant", "content": null })).as_bytes(),
        );
        std::fs::write(temp.path().join("s2.jsonl"), bytes).unwrap();

        let mut b = blob("s2");
        hydrate(&mut b, temp.path()).unwrap();

        assert_eq!(
            b.history,
            vec![
                json!({ "type": "user", "text": "kept" }),
                json!({ "type": "user", "text": "kept2" }),
            ]
        );
        // A truncated context line cannot replace the previous context.
        assert_eq!(b.context, json!({ "stale": true }));
    }

    #[test]
    fn missing_session_keeps_existing_history() {
        let temp = TempDir::new();
        let mut b = blob("fresh");
        let history = b.history.clone();
        hydrate(&mut b, temp.path()).unwrap();
        assert_eq!(b.history, history);
        assert_eq!(b.context, json!({ "stale": true }));
    }

    #[test]
    fn history_is_capped_to_the_newest_entries() {
        let temp = TempDir::new();
        let lines: Vec<Value> = (0..350)
            .map(|index| json!({ "role": "user", "content": format!("m{index}") }))
            .collect();
        write_lines(temp.path(), "s3", &lines);

        let mut b = blob("s3");
        hydrate(&mut b, temp.path()).unwrap();

        assert_eq!(b.history.len(), MAX_UI_ENTRIES);
        assert_eq!(b.history.first().unwrap()["text"], "m50");
        assert_eq!(b.history.last().unwrap()["text"], "m349");
    }

    #[test]
    fn multimodal_context_restores_preview_without_chat_noise() {
        let temp = TempDir::new();
        write_lines(
            temp.path(),
            "vision",
            &[json!({"role":"user","content":[
                {"type":"text","text":"Context for this request (JSON):\n{\"content\":{\"text\":\"code\",\"image\":{\"width\":1}}}"},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,test"}}
            ]})],
        );
        let mut b = blob("vision");
        hydrate(&mut b, temp.path()).unwrap();
        assert!(b.history.is_empty());
        assert_eq!(b.context["content"]["text"], "code");
        assert_eq!(
            b.context["content"]["image"]["data_url"],
            "data:image/png;base64,test"
        );
    }

    #[test]
    fn invalid_blob_id_is_reported() {
        let temp = TempDir::new();
        let mut b = blob("../escape");
        assert!(hydrate(&mut b, temp.path()).is_err());
    }
}
