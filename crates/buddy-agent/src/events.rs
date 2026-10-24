//! UI-facing event helpers.
//!
//! The engine emits one JSON object per event through an unbounded channel.
//! Only the event types listed in `docs/contracts.md` are emitted:
//! `text`, `thinking`, `tool_start`, `tool_result`, `approval`, `error`, `done`.
//! Extra fields (`id`, `risk`, `is_error`) are additive and safe for the daemon
//! to ignore.

use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

/// Send an event, ignoring a closed receiver.
///
/// The UI closing its channel must never make the agent fail mid-turn; the
/// session log remains the source of truth.
pub(crate) fn send(events: &UnboundedSender<Value>, event: Value) {
    let _ = events.send(event);
}

pub(crate) fn text(events: &UnboundedSender<Value>, text: impl Into<String>) {
    send(events, json!({ "type": "text", "text": text.into() }));
}

pub(crate) fn thinking(events: &UnboundedSender<Value>, text: impl Into<String>) {
    send(events, json!({ "type": "thinking", "text": text.into() }));
}

pub(crate) fn tool_start(events: &UnboundedSender<Value>, id: &str, name: &str, arguments: &Value) {
    send(
        events,
        json!({
            "type": "tool_start",
            "id": id,
            "name": name,
            "arguments": arguments,
        }),
    );
}

pub(crate) fn tool_result(
    events: &UnboundedSender<Value>,
    id: &str,
    name: &str,
    text: &str,
    is_error: bool,
) {
    send(
        events,
        json!({
            "type": "tool_result",
            "id": id,
            "name": name,
            "text": text,
            "is_error": is_error,
        }),
    );
}

/// Ask the user to approve one exact tool invocation.
///
/// `arguments` is the exact value the model produced (after JSON decoding), not
/// a summary. The UI must answer with exactly one `bool` on the approvals
/// channel; anything else (closed channel, missing answer) denies.
pub(crate) fn approval(
    events: &UnboundedSender<Value>,
    name: &str,
    arguments: &Value,
    risk: &str,
    preview: Option<&Value>,
) {
    let mut event = json!({
        "type": "approval",
        "name": name,
        "arguments": arguments,
        "risk": risk,
    });
    if let Some(preview) = preview {
        event["preview"] = preview.clone();
    }
    send(events, event);
}

pub(crate) fn error(events: &UnboundedSender<Value>, text: impl Into<String>) {
    send(events, json!({ "type": "error", "text": text.into() }));
}

pub(crate) fn done(events: &UnboundedSender<Value>) {
    send(events, json!({ "type": "done" }));
}
