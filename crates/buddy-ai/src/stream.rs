//! Pure accumulation of OpenAI-compatible chat-completion chunks.
//!
//! Streaming tool calls arrive in fragments: the id and name in the first
//! fragment for an `index`, and partial JSON `arguments` afterwards. This
//! module never parses partial arguments. It concatenates them by index and
//! emits one complete tool-call object per index once the stream ends.
//!
//! The accumulator is deliberately transport-free so it can be exercised with
//! recorded fixtures and mutated chunks in unit tests.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::error::{snippet, ProviderError};
use crate::ProviderEvent;

/// Upper bound for accumulated assistant text.
pub const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
/// Upper bound for accumulated reasoning text.
pub const MAX_REASONING_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound for one tool call's accumulated argument fragments.
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound on parallel tool calls in a single response.
pub const MAX_TOOL_CALLS: usize = 256;

#[derive(Debug, Clone)]
struct PartialToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
    kind: String,
}

impl Default for PartialToolCall {
    fn default() -> Self {
        Self {
            id: None,
            name: String::new(),
            arguments: String::new(),
            kind: "function".to_string(),
        }
    }
}

/// Stateful decoder for one assistant response.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    role: Option<String>,
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    usage: Option<Value>,
    finish_reason: Option<String>,
    saw_chunk: bool,
    saw_choice: bool,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assistant text accumulated so far.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Reasoning / thinking text accumulated so far.
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    /// Usage object from the stream, if the server sent one.
    pub fn usage(&self) -> Option<&Value> {
        self.usage.as_ref()
    }

    /// Last `finish_reason` seen. Deliberately not used to decide whether tools
    /// were called; the accumulated calls are authoritative.
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    /// True once at least one chunk was applied.
    pub fn saw_chunk(&self) -> bool {
        self.saw_chunk
    }

    /// True once a completion choice was seen. A response with only usage or
    /// keepalive chunks never sets this.
    pub fn saw_choice(&self) -> bool {
        self.saw_choice
    }

    /// Number of distinct tool-call indexes seen.
    pub fn tool_call_count(&self) -> usize {
        self.tool_calls.len()
    }

    /// Apply one decoded chunk, returning text/thinking events as they arrive.
    ///
    /// Tool calls and usage are held until [`Self::finalize`], so consumers see
    /// complete calls and a stable ordering.
    pub fn apply(&mut self, chunk: &Value) -> Result<Vec<ProviderEvent>, ProviderError> {
        let object = chunk.as_object().ok_or_else(|| {
            ProviderError::protocol(format!("chunk is not a JSON object: {}", snippet(chunk)))
        })?;

        if let Some(error) = object.get("error").filter(|value| !value.is_null()) {
            return Err(ProviderError::upstream(crate::error::upstream_message(
                error,
            )));
        }

        self.saw_chunk = true;

        if let Some(usage) = object.get("usage").filter(|value| !value.is_null()) {
            self.usage = Some(usage.clone());
        }

        let Some(choices) = object.get("choices") else {
            // Usage-only chunks and keepalives have no choices.
            return Ok(Vec::new());
        };
        let choices = choices.as_array().ok_or_else(|| {
            ProviderError::protocol(format!("choices is not an array: {}", snippet(choices)))
        })?;

        // The request asks for one choice. Prefer index 0 when present, and
        // otherwise fall back to the first object for lenient servers.
        let choice = choices
            .iter()
            .find(|choice| choice.get("index").and_then(Value::as_u64) == Some(0))
            .or_else(|| choices.iter().find(|choice| choice.is_object()));
        let Some(choice) = choice else {
            return Ok(Vec::new());
        };
        self.saw_choice = true;

        let mut events = Vec::new();

        let delta = choice
            .get("delta")
            .or_else(|| choice.get("message"))
            .filter(|value| value.is_object());

        if let Some(delta) = delta {
            let delta = delta.as_object().expect("filtered to object");
            if let Some(role) = delta.get("role").and_then(Value::as_str) {
                if !role.trim().is_empty() {
                    self.role = Some(role.to_string());
                }
            }
            if let Some(content) = delta.get("content") {
                self.consume_text(content, &mut events)?;
            }
            if let Some(reasoning) =
                first_present(delta, &["reasoning_content", "reasoning", "thinking"])
            {
                self.consume_reasoning(reasoning, &mut events)?;
            }
            if let Some(tool_calls) = delta.get("tool_calls") {
                self.consume_tool_calls(tool_calls)?;
            }
        } else if let Some(text) = choice.get("text").and_then(Value::as_str) {
            // Legacy completions-style chunk.
            self.push_text(text, &mut events)?;
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            if !reason.trim().is_empty() {
                self.finish_reason = Some(reason.to_string());
            }
        }

        Ok(events)
    }

    /// Finish the response: generate missing ids, build complete tool calls,
    /// and return the assistant wire message plus deferred events.
    pub fn finalize(&mut self) -> (Value, Vec<ProviderEvent>) {
        let mut events = Vec::new();
        let mut wire_calls = Vec::new();
        let mut used_ids: HashSet<String> = HashSet::new();

        for (index, partial) in self.tool_calls.iter_mut() {
            let id = match partial.id.take() {
                Some(id) if !id.trim().is_empty() => id,
                _ => generated_id(*index),
            };
            let mut id = id;
            while used_ids.contains(&id) {
                id = generated_id(*index);
            }
            used_ids.insert(id.clone());
            partial.id = Some(id.clone());

            let arguments = if partial.arguments.is_empty() {
                "{}".to_string()
            } else {
                partial.arguments.clone()
            };
            let kind = if partial.kind.trim().is_empty() {
                "function"
            } else {
                partial.kind.as_str()
            };
            let call = json!({
                "id": id,
                "type": kind,
                "function": {
                    "name": partial.name.clone(),
                    "arguments": arguments,
                }
            });
            events.push(ProviderEvent::ToolCall(call.clone()));
            wire_calls.push(call);
        }

        if let Some(usage) = &self.usage {
            events.push(ProviderEvent::Usage(usage.clone()));
        }

        let mut message = Map::new();
        message.insert(
            "role".to_string(),
            Value::String(self.role.clone().unwrap_or_else(|| "assistant".to_string())),
        );
        if self.content.is_empty() {
            message.insert(
                "content".to_string(),
                if wire_calls.is_empty() {
                    Value::String(String::new())
                } else {
                    Value::Null
                },
            );
        } else {
            message.insert("content".to_string(), Value::String(self.content.clone()));
        }
        if !wire_calls.is_empty() {
            message.insert("tool_calls".to_string(), Value::Array(wire_calls));
        }

        (Value::Object(message), events)
    }

    fn push_text(
        &mut self,
        text: &str,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        if text.is_empty() {
            return Ok(());
        }
        if self.content.len() + text.len() > MAX_CONTENT_BYTES {
            return Err(ProviderError::protocol(format!(
                "assistant text exceeds {MAX_CONTENT_BYTES} bytes"
            )));
        }
        self.content.push_str(text);
        events.push(ProviderEvent::Text(text.to_string()));
        Ok(())
    }

    fn consume_text(
        &mut self,
        value: &Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        match value {
            Value::Null => Ok(()),
            Value::String(text) => self.push_text(text, events),
            Value::Array(parts) => {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        self.push_text(text, events)?;
                    } else if let Some(text) = part.as_str() {
                        self.push_text(text, events)?;
                    }
                    // Non-text parts (images, audio) are not assistant output.
                }
                Ok(())
            }
            other => Err(ProviderError::protocol(format!(
                "content delta is not text: {}",
                snippet(other)
            ))),
        }
    }

    fn push_reasoning(
        &mut self,
        text: &str,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        if text.is_empty() {
            return Ok(());
        }
        if self.reasoning.len() + text.len() > MAX_REASONING_BYTES {
            return Err(ProviderError::protocol(format!(
                "reasoning text exceeds {MAX_REASONING_BYTES} bytes"
            )));
        }
        self.reasoning.push_str(text);
        events.push(ProviderEvent::Thinking(text.to_string()));
        Ok(())
    }

    fn consume_reasoning(
        &mut self,
        value: &Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        match value {
            Value::Null => Ok(()),
            Value::String(text) => self.push_reasoning(text, events),
            Value::Object(object) => {
                if let Some(text) = object.get("content").and_then(Value::as_str) {
                    self.push_reasoning(text, events)?;
                } else if let Some(text) = object.get("text").and_then(Value::as_str) {
                    self.push_reasoning(text, events)?;
                }
                Ok(())
            }
            Value::Array(parts) => {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        self.push_reasoning(text, events)?;
                    } else if let Some(text) = part.get("content").and_then(Value::as_str) {
                        self.push_reasoning(text, events)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn consume_tool_calls(&mut self, value: &Value) -> Result<(), ProviderError> {
        let calls = value.as_array().ok_or_else(|| {
            ProviderError::protocol(format!("tool_calls is not an array: {}", snippet(value)))
        })?;

        for (position, call) in calls.iter().enumerate() {
            let call = call.as_object().ok_or_else(|| {
                ProviderError::protocol(format!("tool call is not an object: {}", snippet(call)))
            })?;

            let index = call
                .get("index")
                .and_then(parse_index)
                .unwrap_or(if calls.len() == 1 { 0 } else { position });

            if !self.tool_calls.contains_key(&index) && self.tool_calls.len() >= MAX_TOOL_CALLS {
                return Err(ProviderError::protocol(format!(
                    "more than {MAX_TOOL_CALLS} tool calls in one response"
                )));
            }
            let entry = self.tool_calls.entry(index).or_default();

            if let Some(id) = call.get("id").and_then(Value::as_str) {
                let id = id.trim();
                if !id.is_empty() {
                    entry.id = Some(id.to_string());
                }
            }
            if let Some(kind) = call.get("type").and_then(Value::as_str) {
                if !kind.trim().is_empty() {
                    entry.kind = kind.to_string();
                }
            }

            let function = call.get("function").and_then(Value::as_object);
            let name = function
                .and_then(|function| function.get("name"))
                .or_else(|| call.get("name"))
                .and_then(Value::as_str);
            if let Some(name) = name {
                // The name arrives whole in the first fragment. Keep it; later
                // fragments repeat it or omit it.
                if entry.name.is_empty() && !name.is_empty() {
                    entry.name = name.to_string();
                }
            }

            let arguments = function
                .and_then(|function| function.get("arguments"))
                .or_else(|| call.get("arguments"));
            match arguments {
                None | Some(Value::Null) => {}
                Some(Value::String(fragment)) => {
                    if entry.arguments.len() + fragment.len() > MAX_TOOL_ARGUMENT_BYTES {
                        return Err(ProviderError::protocol(format!(
                            "tool call arguments exceed {MAX_TOOL_ARGUMENT_BYTES} bytes"
                        )));
                    }
                    entry.arguments.push_str(fragment);
                }
                Some(other) => {
                    // A few servers send a complete object instead of a JSON
                    // string. Preserve it as JSON text.
                    let text = serde_json::to_string(other).map_err(|error| {
                        ProviderError::protocol(format!(
                            "tool call arguments are not serializable: {error}"
                        ))
                    })?;
                    if text.len() > MAX_TOOL_ARGUMENT_BYTES {
                        return Err(ProviderError::protocol(format!(
                            "tool call arguments exceed {MAX_TOOL_ARGUMENT_BYTES} bytes"
                        )));
                    }
                    entry.arguments = text;
                }
            }
        }

        Ok(())
    }
}

fn first_present<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find(|value| !value.is_null())
}

/// Tool-call indexes are unsigned integers in the specification, but some
/// local servers serialize them as strings. Accept both.
fn parse_index(value: &Value) -> Option<usize> {
    if let Some(index) = value.as_u64() {
        return usize::try_from(index).ok();
    }
    value.as_str()?.trim().parse::<usize>().ok()
}

fn generated_id(index: usize) -> String {
    format!("call_{index}_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_events(events: &[ProviderEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn streams_text_and_reasoning_fragments() {
        let mut accumulator = StreamAccumulator::new();
        let mut events = Vec::new();
        events.extend(
            accumulator
                .apply(
                    &json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}),
                )
                .expect("chunk"),
        );
        events.extend(
            accumulator
                .apply(&json!({"choices":[{"index":0,"delta":{"content":"lo","reasoning_content":"why"}}]}))
                .expect("chunk"),
        );
        events.extend(
            accumulator
                .apply(&json!({"choices":[{"index":0,"delta":{"reasoning_content":" not"}}]}))
                .expect("chunk"),
        );
        assert_eq!(accumulator.content(), "Hello");
        assert_eq!(accumulator.reasoning(), "why not");
        assert_eq!(text_events(&events), "Hello");
        let thinking: String = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::Thinking(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, "why not");
    }

    #[test]
    fn accumulates_tool_call_fragments_by_index() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"call_a","type":"function","function":{"name":"echo","arguments":"{\"te"}},
                {"index":1,"id":"call_b","type":"function","function":{"name":"read","arguments":""}}
            ]}}]}))
            .expect("first");
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":1,"function":{"arguments":"{\"path\":"}},
                {"index":0,"function":{"arguments":"xt\":\"hi\"}"}}
            ]}}]}))
            .expect("second");
        accumulator
            .apply(&json!({"choices":[{"index":0,"finish_reason":"tool_calls","delta":{}}]}))
            .expect("finish");

        let (message, events) = accumulator.finalize();
        let calls = message["tool_calls"].as_array().expect("tool calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_a");
        assert_eq!(calls[0]["function"]["name"], "echo");
        assert_eq!(calls[0]["function"]["arguments"], "{\"text\":\"hi\"}");
        assert_eq!(calls[1]["id"], "call_b");
        assert_eq!(calls[1]["function"]["arguments"], "{\"path\":");

        // Tool calls are emitted in index order, parsed only now.
        let emitted: Vec<&Value> = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0]["id"], "call_a");
    }

    #[test]
    fn generates_unique_ids_when_missing_or_duplicated() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"function":{"name":"a","arguments":"{}"}},
                {"index":1,"function":{"name":"b","arguments":"{}"}}
            ]}}]}))
            .expect("chunk");
        let (message, _) = accumulator.finalize();
        let calls = message["tool_calls"].as_array().expect("calls");
        let first = calls[0]["id"].as_str().expect("id");
        let second = calls[1]["id"].as_str().expect("id");
        assert!(first.starts_with("call_"));
        assert!(second.starts_with("call_"));
        assert_ne!(first, second);

        let mut duplicate = StreamAccumulator::new();
        duplicate
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"same","function":{"name":"a","arguments":"{}"}},
                {"index":1,"id":"same","function":{"name":"b","arguments":"{}"}}
            ]}}]}))
            .expect("chunk");
        let (message, _) = duplicate.finalize();
        let calls = message["tool_calls"].as_array().expect("calls");
        assert_ne!(calls[0]["id"], calls[1]["id"]);
    }

    #[test]
    fn usage_only_chunk_is_deferred_to_finalize() {
        let mut accumulator = StreamAccumulator::new();
        let events = accumulator
            .apply(&json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4}}))
            .expect("usage");
        assert!(events.is_empty());
        assert_eq!(accumulator.usage().expect("usage")["completion_tokens"], 4);
        let (_, events) = accumulator.finalize();
        assert!(matches!(&events[0], ProviderEvent::Usage(value) if value["prompt_tokens"] == 3));
    }

    #[test]
    fn empty_content_is_null_only_when_tools_exist() {
        let mut empty = StreamAccumulator::new();
        let (message, _) = empty.finalize();
        assert_eq!(message["content"], "");
        assert!(message.get("tool_calls").is_none());

        let mut with_tool = StreamAccumulator::new();
        with_tool
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"x","function":{"name":"f","arguments":""}}
            ]}}]}))
            .expect("chunk");
        let (message, _) = with_tool.finalize();
        assert!(message["content"].is_null());
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"], "{}",
            "empty fragments become an empty argument object"
        );
    }

    #[test]
    fn top_level_error_is_bounded() {
        let mut accumulator = StreamAccumulator::new();
        let error = accumulator
            .apply(&json!({"error":{"message":"x".repeat(5000)}}))
            .expect_err("error");
        assert!(matches!(error, ProviderError::Upstream(_)));
        assert!(error.to_string().chars().count() < 1000);
    }

    #[test]
    fn malformed_shapes_are_errors_not_panics() {
        for chunk in [
            json!("not an object"),
            json!({"choices": 5}),
            json!({"choices":[{"delta":{"content":42}}]}),
            json!({"choices":[{"delta":{"tool_calls":"nope"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[7]}}]}),
        ] {
            let mut accumulator = StreamAccumulator::new();
            assert!(accumulator.apply(&chunk).is_err(), "chunk: {chunk}");
        }
    }

    #[test]
    fn missing_index_uses_single_call_slot() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(&json!({"choices":[{"delta":{"tool_calls":[
                {"id":"c","function":{"name":"f","arguments":"{\"a\":"}}
            ]}}]}))
            .expect("first");
        accumulator
            .apply(&json!({"choices":[{"delta":{"tool_calls":[
                {"function":{"arguments":"1}"}}
            ]}}]}))
            .expect("second");
        let (message, _) = accumulator.finalize();
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
    }

    #[test]
    fn content_parts_and_object_arguments_are_supported() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"content":[
                {"type":"text","text":"a"},{"type":"image_url","image_url":{"url":"x"}}
            ]}}]}))
            .expect("parts");
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"id","function":{"name":"f","arguments":{"a":1}}}
            ]}}]}))
            .expect("object args");
        let (message, _) = accumulator.finalize();
        assert_eq!(message["content"], "a");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
    }

    #[test]
    fn string_indexes_accumulate_across_chunks() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":"1","id":"c","function":{"name":"f","arguments":"{\"a\":"}}
            ]}}]}))
            .expect("first");
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":"1","function":{"arguments":"1}"}}
            ]}}]}))
            .expect("second");
        let (message, _) = accumulator.finalize();
        assert_eq!(message["tool_calls"].as_array().expect("calls").len(), 1);
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
    }

    #[test]
    fn choice_less_streams_are_detectable() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(&json!({"choices":[],"usage":{"total_tokens":1}}))
            .expect("usage");
        assert!(!accumulator.saw_choice());
        accumulator
            .apply(&json!({"choices":[{"index":0,"delta":{}}]}))
            .expect("choice");
        assert!(accumulator.saw_choice());
    }

    #[test]
    fn finish_reason_does_not_decide_tools() {
        let mut accumulator = StreamAccumulator::new();
        accumulator
            .apply(
                &json!({"choices":[{"index":0,"finish_reason":"stop","delta":{"tool_calls":[
                    {"index":0,"id":"c","function":{"name":"f","arguments":"{}"}}
                ]}}]}),
            )
            .expect("chunk");
        assert_eq!(accumulator.finish_reason(), Some("stop"));
        let (message, _) = accumulator.finalize();
        assert_eq!(message["tool_calls"].as_array().expect("calls").len(), 1);
    }
}
