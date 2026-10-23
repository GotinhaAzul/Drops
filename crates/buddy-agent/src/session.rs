//! JSONL sessions and OpenAI wire-message helpers.
//!
//! The file is append-only and one JSON object per line. The first line is a
//! session header; subsequent message entries look like
//! `{"type":"message","timestamp":...,"message":{"role":...}}`. Plain message
//! lines (`{"role":...}`) are accepted when loading, so older/hand-written logs
//! remain usable.
//!
//! Only OpenAI wire fields are sent to the provider. Extra bookkeeping fields
//! that may be present in the log are stripped by [`wire_message`].

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use tokio::sync::{Mutex, OwnedMutexGuard};

const MAX_SESSION_ID_BYTES: usize = 128;
const MAX_HISTORY_MESSAGES: usize = 200;
const MAX_HISTORY_CHARS: usize = 400_000;

/// Validate a session id against path traversal and accidental separators.
pub(crate) fn validate_session_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("session id must not be empty");
    }
    if id.len() > MAX_SESSION_ID_BYTES {
        bail!("session id is longer than {MAX_SESSION_ID_BYTES} bytes");
    }
    if id == "." || id == ".." || id.contains("..") {
        bail!("session id must not contain `..`");
    }
    let mut chars = id.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_alphanumeric() {
        bail!("session id must start with an ASCII letter or digit");
    }
    for ch in id.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.') {
            bail!("session id may contain only ASCII letters, digits, `-`, `_` and `.`");
        }
    }
    Ok(())
}

fn session_locks() -> &'static StdMutex<HashMap<PathBuf, Arc<Mutex<()>>>> {
    static LOCKS: OnceLock<StdMutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Serialize runs that share a session file within this process.
pub(crate) async fn lock(path: &Path) -> OwnedMutexGuard<()> {
    let cell = {
        let mut map = session_locks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    cell.lock_owned().await
}

/// Append-only session log.
pub(crate) struct SessionLog {
    path: PathBuf,
}

impl SessionLog {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Write a session header if the file is missing or empty.
    pub(crate) fn ensure_header(&self, session_id: &str, root: &Path) -> Result<()> {
        let empty = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata.len() == 0,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("inspecting session {}", self.path.display()))
            }
        };
        if empty {
            let created = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            self.append(&json!({
                "type": "session",
                "version": 1,
                "id": session_id,
                "created_unix": created,
                "cwd": root,
            }))?;
        }
        Ok(())
    }

    /// Load message entries, skipping malformed lines rather than failing the
    /// whole session. Truncated final lines therefore cannot brick a session.
    pub(crate) fn load(&self) -> Result<Vec<Value>> {
        let text = match std::fs::read(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("reading session {}", self.path.display()))
            }
        };

        let mut messages = Vec::new();
        for line in text.split(|byte| *byte == b'\n') {
            // Torn UTF-8 and malformed final records cannot brick the session.
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            if let Some(message) = value.get("message") {
                if message.get("role").and_then(Value::as_str).is_some() {
                    messages.push(message.clone());
                    continue;
                }
            }
            if value.get("role").and_then(Value::as_str).is_some() {
                messages.push(value);
            }
        }
        Ok(messages)
    }

    pub(crate) fn append_message(&self, message: &Value) -> Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        self.append(&json!({
            "type": "message",
            "timestamp": timestamp,
            "message": message,
        }))
    }

    fn append(&self, value: &Value) -> Result<()> {
        let line = serde_json::to_string(value).context("serializing session entry")?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening session {}", self.path.display()))?;
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut tail = [0];
            file.read_exact(&mut tail)?;
            if tail[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        file.write_all(line.as_bytes())
            .with_context(|| format!("writing session {}", self.path.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("writing session {}", self.path.display()))?;
        file.flush()
            .with_context(|| format!("flushing session {}", self.path.display()))?;
        Ok(())
    }
}

/// A decoded tool call from an assistant message.
pub(crate) struct ParsedCall {
    pub id: String,
    pub name: Option<String>,
    /// Fully decoded arguments, or a human-readable parse error.
    pub arguments: std::result::Result<Value, String>,
    /// Best-effort arguments for display in `tool_start`/approval events.
    pub display_arguments: Value,
}

impl ParsedCall {
    /// Model-facing tool message. Error text is prefixed because OpenAI tool
    /// messages have no error flag.
    pub(crate) fn tool_message(&self, content: &str, is_error: bool) -> Value {
        let content = if is_error && !content.starts_with("Error:") {
            format!("Error: {content}")
        } else {
            content.to_string()
        };
        let mut message = Map::new();
        message.insert("role".into(), json!("tool"));
        message.insert("tool_call_id".into(), json!(self.id));
        if let Some(name) = self.name.as_deref().filter(|name| !name.is_empty()) {
            message.insert("name".into(), json!(name));
        }
        message.insert("content".into(), json!(content));
        Value::Object(message)
    }
}

/// Result of executing one tool call.
pub(crate) enum CallOutcome {
    Finished { content: String, is_error: bool },
    Cancelled,
}

/// A tool call whose result has not been recorded yet.
struct PendingCall {
    id: String,
    name: String,
}

/// Model-facing result for a tool call that was cut off before it produced one.
const INTERRUPTED_TOOL_RESULT: &str = "Error: This tool call was interrupted before a result was recorded (the agent stopped while the call was running). Its outcome is unknown; re-issue the call if its result is still needed.";

/// History with any interrupted tool batches completed.
pub(crate) struct HistoryRepair {
    /// The repaired history, ready for [`trim_history`] and [`wire_message`].
    pub messages: Vec<Value>,
    /// Synthetic tool results that are not in the log yet, in wire order.
    ///
    /// Only a batch that is still open at the *end* of the history can be
    /// completed in place, so only those results are returned for appending.
    /// A batch interrupted in the middle (for example by a corrupt line) is
    /// completed in memory for this request: an append-only log cannot insert
    /// the missing result next to its call, and adding it at the end would
    /// leave the original batch dangling again on the next load. Repair is
    /// deterministic, so the in-memory result is identical every time.
    pub appended: Vec<Value>,
}

/// Complete tool-call batches that a crash or lost log line interrupted.
///
/// A hard stop between appending an assistant message and appending its tool
/// results leaves `tool_calls` with no `tool` replies. Strict OpenAI endpoints
/// reject a follow-up turn in that state with a 400, so the history must be
/// repaired before the next user message is sent.
///
/// The whole history is scanned, not just the tail: a truncated or corrupt line
/// in the middle (skipped by [`SessionLog::load`]) can also leave an
/// unanswered batch behind. For every assistant batch each call id receives
/// exactly one tool result. Real results are kept in their original position,
/// orphan and duplicate tool messages are excluded from the wire history, and
/// missing results are synthesized as errors. Complete valid turns are left
/// untouched, and repeating the repair over its own output adds nothing.
pub(crate) fn repair_history(history: Vec<Value>) -> HistoryRepair {
    let mut messages = Vec::with_capacity(history.len());
    let mut appended = Vec::new();
    let mut pending: Vec<PendingCall> = Vec::new();

    for message in history {
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                flush_interrupted(&mut messages, &mut appended, &mut pending, false);
                let (message, calls) = repaired_batch(message);
                pending = calls;
                messages.push(message);
            }
            Some("tool") => {
                let id = wire_tool_call_id(&message);
                let matched = id.and_then(|id| {
                    pending
                        .iter()
                        .position(|call| call.id == id)
                        .map(|position| pending.remove(position))
                });
                if matched.is_some() {
                    messages.push(message);
                }
                // Orphan, duplicate, or malformed results are not sent
                // upstream; the real log line is left in place.
            }
            _ => {
                flush_interrupted(&mut messages, &mut appended, &mut pending, false);
                messages.push(message);
            }
        }
    }
    flush_interrupted(&mut messages, &mut appended, &mut pending, true);

    HistoryRepair { messages, appended }
}

/// Emit one synthetic error result per still-unanswered call, in call order.
///
/// `persist` is set only at the end of the history, where an appended result
/// is adjacent to its call and will satisfy repair on the next load.
fn flush_interrupted(
    messages: &mut Vec<Value>,
    appended: &mut Vec<Value>,
    pending: &mut Vec<PendingCall>,
    persist: bool,
) {
    for call in pending.drain(..) {
        let message = interrupted_tool_message(&call);
        if persist {
            appended.push(message.clone());
        }
        messages.push(message);
    }
}

fn interrupted_tool_message(call: &PendingCall) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": call.id,
        "name": call.name,
        "content": INTERRUPTED_TOOL_RESULT,
    })
}

/// Mirror [`wire_message`]'s assistant-call filtering and drop duplicate ids.
///
/// Returns the message to keep plus the calls that need results. A batch that
/// only holds calls the wire cannot represent becomes a plain assistant
/// message, so following tool messages are treated as orphans.
fn repaired_batch(message: Value) -> (Value, Vec<PendingCall>) {
    let calls = match message.get("tool_calls").and_then(Value::as_array) {
        Some(calls) => calls.clone(),
        None => return (message, Vec::new()),
    };

    let mut kept = Vec::with_capacity(calls.len());
    let mut pending = Vec::with_capacity(calls.len());
    let mut seen: Vec<String> = Vec::new();
    for call in &calls {
        let Some(function) = call.get("function") else {
            continue;
        };
        let Some(name) = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if seen.iter().any(|seen| seen == id) {
            continue;
        }
        seen.push(id.to_string());
        kept.push(call.clone());
        pending.push(PendingCall {
            id: id.to_string(),
            name: name.to_string(),
        });
    }

    if kept.len() == calls.len() {
        return (message, pending);
    }

    let Value::Object(mut object) = message else {
        return (message, Vec::new());
    };
    if kept.is_empty() {
        object.remove("tool_calls");
    } else {
        object.insert("tool_calls".into(), Value::Array(kept));
    }
    (Value::Object(object), pending)
}

/// The id [`wire_message`] would send for this tool message, if any.
fn wire_tool_call_id(message: &Value) -> Option<&str> {
    if matches!(message.get("content"), Some(Value::Array(_))) {
        return None;
    }
    message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

/// Normalize a provider assistant message into the shape persisted and replayed.
///
/// Missing tool-call ids are generated, flat `{name, arguments}` calls are
/// lifted into the OpenAI `function` envelope, and object arguments are
/// serialized. Truncated/malformed argument strings are preserved so the loop
/// can report them to the model instead of crashing.
pub(crate) fn normalize_assistant(value: Value) -> Value {
    let mut object = match value {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        Value::String(text) => {
            let mut object = Map::new();
            object.insert("content".into(), Value::String(text));
            object
        }
        other => {
            let mut object = Map::new();
            object.insert("content".into(), Value::String(other.to_string()));
            object
        }
    };

    object.insert("role".into(), json!("assistant"));

    let calls = object
        .remove("tool_calls")
        .and_then(|calls| calls.as_array().cloned())
        .unwrap_or_default();
    let normalized: Vec<Value> = calls.into_iter().filter_map(normalize_tool_call).collect();
    if !normalized.is_empty() {
        object.insert("tool_calls".into(), Value::Array(normalized));
    }

    match object.get("content") {
        None | Some(Value::Null) => {
            object.insert("content".into(), json!(""));
        }
        Some(Value::String(_)) | Some(Value::Array(_)) => {}
        Some(other) => {
            let text = other.to_string();
            object.insert("content".into(), Value::String(text));
        }
    }

    Value::Object(object)
}

fn normalize_tool_call(call: Value) -> Option<Value> {
    if !call.is_object() {
        return Some(json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "function",
            "function": { "name": "", "arguments": "{}" }
        }));
    }

    let id = call
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let function = call.get("function").unwrap_or(&call);
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let arguments = match function.get("arguments") {
        None | Some(Value::Null) => "{}".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };

    Some(json!({
        "id": id,
        "type": "function",
        "function": { "name": name, "arguments": arguments }
    }))
}

/// Decode the calls in a normalized assistant message.
pub(crate) fn tool_calls(assistant: &Value) -> Vec<ParsedCall> {
    assistant
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().map(parse_call).collect())
        .unwrap_or_default()
}

fn parse_call(call: &Value) -> ParsedCall {
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let function = call.get("function").unwrap_or(call);
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);

    let raw = function
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!("{}"));

    let arguments = match &raw {
        Value::Null => Ok(json!({})),
        Value::Object(_) => Ok(raw.clone()),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) if value.is_object() => Ok(value),
            Ok(_) => Err("arguments must decode to a JSON object".to_string()),
            Err(err) => Err(err.to_string()),
        },
        _ => Err("arguments must be a JSON object or a JSON object string".to_string()),
    };

    let display_arguments = match &arguments {
        Ok(value) => value.clone(),
        Err(_) => raw,
    };

    ParsedCall {
        id,
        name,
        arguments,
        display_arguments,
    }
}

/// Keep a session bounded before sending it upstream.
///
/// The newest messages win. Leading orphan tool messages and assistant messages
/// whose tool results were trimmed away are dropped so the request is valid
/// OpenAI wire data.
pub(crate) fn trim_history(mut history: Vec<Value>) -> Vec<Value> {
    if history.len() > MAX_HISTORY_MESSAGES {
        history.drain(..history.len() - MAX_HISTORY_MESSAGES);
    }

    let mut start = history.len();
    let mut total = 0usize;
    for index in (0..history.len()).rev() {
        total = total.saturating_add(approx_chars(&history[index]));
        start = index;
        if total > MAX_HISTORY_CHARS && index + 1 < history.len() {
            start = index + 1;
            break;
        }
    }
    let mut trimmed = history.split_off(start.min(history.len()));

    loop {
        let first = trimmed.first().and_then(|message| {
            message
                .get("role")
                .and_then(Value::as_str)
                .map(|role| (role, message))
        });
        match first {
            Some(("tool", _)) => {
                trimmed.remove(0);
            }
            Some(("assistant", message)) if message.get("tool_calls").is_some() => {
                trimmed.remove(0);
            }
            _ => break,
        }
    }
    trimmed
}

fn approx_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.len())
        .unwrap_or(0)
}

/// Convert one persisted message to a strict OpenAI wire message.
pub(crate) fn wire_message(message: &Value) -> Option<Value> {
    let role = message.get("role").and_then(Value::as_str)?;
    match role {
        "system" | "user" => {
            let content = match message.get("content") {
                None | Some(Value::Null) => json!(""),
                Some(content) => content.clone(),
            };
            Some(json!({ "role": role, "content": content }))
        }
        "assistant" => {
            let content = match message.get("content") {
                None | Some(Value::Null) => json!(""),
                Some(content) => content.clone(),
            };
            let mut wire = Map::new();
            wire.insert("role".into(), json!("assistant"));
            wire.insert("content".into(), content);
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                let calls: Vec<Value> = calls
                    .iter()
                    .filter_map(|call| {
                        let function = call.get("function")?;
                        let name = function.get("name")?.as_str()?;
                        if name.trim().is_empty() {
                            return None;
                        }
                        let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                        let arguments = match function.get("arguments") {
                            Some(Value::String(text)) => Value::String(text.clone()),
                            Some(other) => Value::String(other.to_string()),
                            None => Value::String("{}".to_string()),
                        };
                        Some(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": arguments }
                        }))
                    })
                    .collect();
                if !calls.is_empty() {
                    wire.insert("tool_calls".into(), Value::Array(calls));
                }
            }
            Some(Value::Object(wire))
        }
        "tool" => {
            let tool_call_id = message.get("tool_call_id").and_then(Value::as_str)?;
            let content = match message.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(_)) => return None,
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let mut wire = Map::new();
            wire.insert("role".into(), json!("tool"));
            wire.insert("tool_call_id".into(), json!(tool_call_id));
            if let Some(name) = message.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    wire.insert("name".into(), json!(name));
                }
            }
            wire.insert("content".into(), json!(content));
            Some(Value::Object(wire))
        }
        _ => None,
    }
}

/// Whether a context bundle should be skipped entirely.
pub(crate) fn is_empty_context(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(value) => !value,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::String(text) => text.trim().is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn log() -> (TempDir, SessionLog) {
        let temp = TempDir::new();
        let log = SessionLog::new(temp.path().join("s1.jsonl"));
        (temp, log)
    }

    #[test]
    fn session_ids_accept_expected_ids_and_reject_traversal() {
        for id in ["s1", "abc-123", "a.b_c", "0"] {
            assert!(validate_session_id(id).is_ok(), "{id}");
        }
        for id in [
            "", ".", "..", "../x", "a/b", "a\\b", "a b", "-a", "_a", ".a", "a..b", "é",
        ] {
            assert!(validate_session_id(id).is_err(), "{id}");
        }
        let long = "a".repeat(MAX_SESSION_ID_BYTES + 1);
        assert!(validate_session_id(&long).is_err());
    }

    #[test]
    fn session_path_stays_directly_under_dir() {
        let dir = Path::new("/tmp/sessions");
        let path = crate::session_path(dir, "abc").unwrap();
        assert_eq!(path, dir.join("abc.jsonl"));
        assert!(crate::session_path(dir, "../abc").is_err());
        assert!(crate::session_path(dir, "abc/def").is_err());
    }

    #[test]
    fn session_round_trips_and_skips_bad_lines() {
        let (_temp, log) = log();
        log.ensure_header("s1", Path::new("/project")).unwrap();
        let user = json!({"role":"user","content":"hello"});
        let assistant = json!({"role":"assistant","content":"hi"});
        log.append_message(&user).unwrap();
        log.append_message(&assistant).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(log.path())
            .unwrap()
            .write_all(b"not json\n{\"role\":\"user\",\"content\":\"partial")
            .unwrap();

        let loaded = log.load().unwrap();
        assert_eq!(loaded, vec![user, assistant]);
        assert!(std::fs::read_to_string(log.path())
            .unwrap()
            .contains("\"type\":\"session\""));
        // A crash can split a multibyte character. New appended records must
        // start on their own line while retaining the original byte prefix.
        std::fs::OpenOptions::new()
            .append(true)
            .open(log.path())
            .unwrap()
            .write_all(&[0xe2, 0x82])
            .unwrap();
        let prefix = std::fs::read(log.path()).unwrap();
        let resumed = json!({"role":"user","content":"resumed"});
        log.append_message(&resumed).unwrap();
        assert!(std::fs::read(log.path()).unwrap().starts_with(&prefix));
        assert_eq!(
            log.load().unwrap(),
            vec![
                json!({"role":"user","content":"hello"}),
                json!({"role":"assistant","content":"hi"}),
                resumed
            ]
        );
    }

    #[test]
    fn header_is_written_once() {
        let (_temp, log) = log();
        log.ensure_header("s1", Path::new("/project")).unwrap();
        log.ensure_header("s1", Path::new("/project")).unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn wire_messages_strip_extra_fields() {
        let message = json!({
            "role":"assistant",
            "content":"",
            "timestamp":123,
            "usage":{"input_tokens":1},
            "tool_calls":[{
                "id":"call_1",
                "type":"function",
                "extra":true,
                "function":{"name":"read","arguments":{"path":"a"},"x":1}
            }]
        });
        let wire = wire_message(&message).unwrap();
        assert_eq!(wire["role"], "assistant");
        assert!(wire.get("timestamp").is_none());
        assert!(wire.get("usage").is_none());
        assert_eq!(
            wire["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert!(wire["tool_calls"][0].get("extra").is_none());
        assert!(wire["tool_calls"][0]["function"].get("x").is_none());
    }

    #[test]
    fn tool_messages_require_ids_and_prefix_errors() {
        assert!(wire_message(&json!({"role":"tool","content":"x"})).is_none());
        let call = ParsedCall {
            id: "call_1".into(),
            name: Some("read".into()),
            arguments: Ok(json!({})),
            display_arguments: json!({}),
        };
        let message = call.tool_message("nope", true);
        assert_eq!(message["content"], "Error: nope");
        assert_eq!(message["tool_call_id"], "call_1");
        assert_eq!(wire_message(&message).unwrap()["content"], "Error: nope");
    }

    #[test]
    fn normalizes_flat_calls_and_generates_ids() {
        let assistant = normalize_assistant(json!({
            "content": null,
            "tool_calls": [{"name":"read","arguments":{"path":"a.txt"}}]
        }));
        let calls = tool_calls(&assistant);
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].id.is_empty());
        assert_eq!(calls[0].name.as_deref(), Some("read"));
        assert_eq!(calls[0].arguments.as_ref().unwrap()["path"], "a.txt");
        assert_eq!(assistant["content"], "");
    }

    #[test]
    fn malformed_arguments_are_reported_not_panicked() {
        let assistant = normalize_assistant(json!({
            "tool_calls": [{
                "id":"c1",
                "function":{"name":"write","arguments":"{\"path\": "}
            }]
        }));
        let calls = tool_calls(&assistant);
        assert!(calls[0].arguments.is_err());
        assert_eq!(calls[0].display_arguments, json!("{\"path\": "));
    }

    #[test]
    fn trims_history_and_drops_orphans() {
        let mut messages = Vec::new();
        messages.push(json!({"role":"tool","tool_call_id":"gone","content":"orphan"}));
        messages.push(json!({"role":"user","content":"u1"}));
        messages.push(json!({"role":"assistant","content":"a1"}));
        messages.push(json!({"role":"user","content":"u2"}));
        let trimmed = trim_history(messages);
        assert_eq!(trimmed.first().unwrap()["content"], "u1");
        assert_eq!(trimmed.len(), 3);

        let mut leading_assistant = Vec::new();
        leading_assistant.push(json!({
            "role":"assistant","content":"",
            "tool_calls":[{"id":"c","type":"function","function":{"name":"read","arguments":"{}"}}]
        }));
        leading_assistant.push(json!({"role":"tool","tool_call_id":"c","content":"x"}));
        leading_assistant.push(json!({"role":"user","content":"next"}));
        let trimmed = trim_history(leading_assistant);
        assert_eq!(trimmed.len(), 1);
        assert_eq!(trimmed[0]["content"], "next");
    }

    fn assistant_with_calls(ids: &[(&str, &str)]) -> Value {
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": ids
                .iter()
                .map(|(id, name)| json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": "{}"}
                }))
                .collect::<Vec<_>>(),
        })
    }

    fn tool_result(id: &str, content: &str) -> Value {
        json!({"role":"tool","tool_call_id":id,"content":content})
    }

    fn strict(messages: &[Value]) {
        let wire: Vec<Value> = messages.iter().filter_map(wire_message).collect();
        crate::testutil::assert_strict_wire(&wire);
    }

    #[test]
    fn repair_completes_a_trailing_interrupted_batch() {
        // The daemon died mid-tool: assistant tool_calls were appended, their
        // results were not.
        let history = vec![
            json!({"role":"user","content":"run it"}),
            assistant_with_calls(&[("c1", "read"), ("c2", "write")]),
        ];

        let repaired = repair_history(history);
        assert_eq!(repaired.appended.len(), 2);
        assert_eq!(repaired.messages.len(), 4);
        let results: Vec<&Value> = repaired.messages.iter().skip(2).collect();
        assert_eq!(results[0]["tool_call_id"], "c1");
        assert_eq!(results[0]["name"], "read");
        assert_eq!(results[1]["tool_call_id"], "c2");
        assert_eq!(results[1]["name"], "write");
        for result in results {
            let content = result["content"].as_str().unwrap();
            assert!(content.starts_with("Error:"), "{content}");
            assert!(content.contains("interrupted"), "{content}");
        }
        assert_eq!(repaired.appended, repaired.messages[2..].to_vec());
        strict(&repaired.messages);

        // Repairing its own output must not add a second result per call.
        let again = repair_history(repaired.messages.clone());
        assert!(again.appended.is_empty());
        assert_eq!(again.messages, repaired.messages);
        strict(&again.messages);
    }

    #[test]
    fn repair_keeps_real_results_and_fills_only_missing_ones() {
        let history = vec![
            json!({"role":"user","content":"three steps"}),
            assistant_with_calls(&[("c1", "read"), ("c2", "write"), ("c3", "edit")]),
            tool_result("c2", "real result for c2"),
        ];

        let repaired = repair_history(history);
        assert_eq!(repaired.messages.len(), 5);
        assert_eq!(repaired.appended.len(), 2);
        assert_eq!(repaired.appended[0]["tool_call_id"], "c1");
        assert_eq!(repaired.appended[1]["tool_call_id"], "c3");
        assert_eq!(repaired.messages[2]["content"], "real result for c2");
        assert_eq!(repaired.messages[2]["tool_call_id"], "c2");

        let ids: Vec<&str> = repaired.messages[2..]
            .iter()
            .map(|message| message["tool_call_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c2", "c1", "c3"]);
        strict(&repaired.messages);
    }

    #[test]
    fn repair_completes_a_batch_interrupted_before_a_later_turn() {
        let history = vec![
            json!({"role":"user","content":"first"}),
            assistant_with_calls(&[("c1", "read"), ("c2", "read")]),
            tool_result("c1", "kept"),
            json!({"role":"user","content":"second"}),
            json!({"role":"assistant","content":"final"}),
        ];

        let repaired = repair_history(history);
        // Mid-history results cannot be inserted into an append-only log, so
        // repair completes them in memory only.
        assert!(repaired.appended.is_empty());
        assert_eq!(repaired.messages.len(), 6);
        let synthetic = &repaired.messages[3];
        assert_eq!(synthetic["role"], "tool");
        assert_eq!(synthetic["tool_call_id"], "c2");
        assert!(synthetic["content"]
            .as_str()
            .unwrap()
            .contains("interrupted"));
        // The synthetic result lands before the next user turn.
        assert_eq!(repaired.messages[4]["role"], "user");
        strict(&repaired.messages);

        // Re-running repair over the repaired history produces the same
        // in-memory completion, not duplicates.
        let again = repair_history(repaired.messages.clone());
        assert!(again.appended.is_empty());
        assert_eq!(again.messages, repaired.messages);
        strict(&again.messages);
    }

    #[test]
    fn repair_excludes_orphan_and_duplicate_results_from_the_wire() {
        let history = vec![
            tool_result("stray", "no assistant asked for this"),
            json!({"role":"user","content":"go"}),
            assistant_with_calls(&[("c1", "read")]),
            tool_result("c1", "real"),
            tool_result("c1", "duplicate"),
            json!({"role":"user","content":"next"}),
        ];

        let repaired = repair_history(history);
        assert!(repaired.appended.is_empty());
        assert_eq!(repaired.messages.len(), 4);
        assert_eq!(repaired.messages[0]["content"], "go");
        assert_eq!(repaired.messages[2]["content"], "real");
        assert_eq!(repaired.messages[3]["content"], "next");
        strict(&repaired.messages);
    }

    #[test]
    fn repair_leaves_complete_valid_turns_untouched() {
        let history = vec![
            json!({"role":"user","content":"go"}),
            assistant_with_calls(&[("c1", "read")]),
            tool_result("c1", "ok"),
            json!({"role":"assistant","content":"done"}),
        ];

        let repaired = repair_history(history.clone());
        assert!(repaired.appended.is_empty());
        assert_eq!(repaired.messages, history);
        strict(&repaired.messages);
    }

    #[test]
    fn repair_deduplicates_repeated_call_ids() {
        let history = vec![
            assistant_with_calls(&[("c1", "read"), ("c1", "read"), ("", "write")]),
            tool_result("c1", "ok"),
            json!({"role":"user","content":"next"}),
        ];

        let repaired = repair_history(history);
        assert!(repaired.appended.is_empty());
        let calls = repaired.messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "c1");
        strict(&repaired.messages);
    }

    #[test]
    fn empty_context_detection() {
        assert!(is_empty_context(&json!(null)));
        assert!(is_empty_context(&json!({})));
        assert!(is_empty_context(&json!([])));
        assert!(is_empty_context(&json!("  ")));
        assert!(!is_empty_context(&json!({"window":{"class":"kitty"}})));
        assert!(!is_empty_context(&json!("text")));
    }
}
