//! Pure request construction.
//!
//! Kept separate from HTTP so the wire shape can be asserted without a server.

use serde_json::{json, Map, Value};

use crate::ProviderConfig;

/// Resolve the chat-completions endpoint from a configured base URL.
///
/// Accepts both a root (`http://host:11434/v1`) and an already-complete
/// endpoint, so copy-pasted configuration keeps working.
pub fn endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

/// Build an OpenAI-compatible chat-completions request body.
///
/// `messages` and `tools` are OpenAI wire objects. Tools are only included when
/// the endpoint is declared tool-capable and at least one tool exists. Usage is
/// requested on streams but servers that ignore `stream_options` are fine.
pub fn build_request_body(
    config: &ProviderConfig,
    messages: &[Value],
    tools: &[Value],
    stream: bool,
) -> Value {
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(config.model.clone()));
    body.insert("messages".to_string(), Value::Array(messages.to_vec()));
    body.insert("stream".to_string(), Value::Bool(stream));
    if stream {
        body.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }
    if config.tools && !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools.to_vec()));
    }
    Value::Object(body)
}

/// Add an output-token limit using the endpoint's declared parameter name.
///
/// [`build_request_body`] does not apply this because `ProviderConfig` carries
/// no limit value; callers with a per-request limit can apply it here.
pub fn apply_max_tokens(body: &mut Value, config: &ProviderConfig, max_tokens: u32) {
    if let Some(object) = body.as_object_mut() {
        object.insert(
            config.max_tokens_key().to_string(),
            Value::Number(max_tokens.into()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderConfig;

    fn config() -> ProviderConfig {
        ProviderConfig {
            base_url: "http://localhost:11434/v1/".into(),
            model: "qwen".into(),
            tools: true,
            ..ProviderConfig::default()
        }
    }

    #[test]
    fn endpoint_handles_roots_and_full_urls() {
        assert_eq!(
            endpoint("http://localhost:11434/v1"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://localhost:11434/v1/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            endpoint("  http://example.test/v1///  "),
            "http://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn request_requests_streaming_usage_and_tools() {
        let messages = vec![json!({"role":"user","content":"hi"})];
        let tools = vec![json!({"type":"function","function":{"name":"echo"}})];
        let body = build_request_body(&config(), &messages, &tools, true);
        assert_eq!(body["model"], "qwen");
        assert_eq!(body["messages"][0]["content"], "hi");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["tools"][0]["function"]["name"], "echo");
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn tools_are_omitted_when_disabled_or_empty() {
        let messages = vec![json!({"role":"user","content":"hi"})];
        let tools = vec![json!({"type":"function","function":{"name":"echo"}})];

        let mut disabled = config();
        disabled.tools = false;
        assert!(build_request_body(&disabled, &messages, &tools, false)
            .get("tools")
            .is_none());
        assert!(build_request_body(&config(), &messages, &[], false)
            .get("tools")
            .is_none());

        let body = build_request_body(&config(), &messages, &tools, false);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn max_tokens_uses_declared_name() {
        let mut body = build_request_body(&config(), &[], &[], true);
        apply_max_tokens(&mut body, &config(), 256);
        assert_eq!(body["max_tokens"], 256);

        let config = ProviderConfig {
            max_tokens_param: "max_completion_tokens".into(),
            ..config()
        };
        let mut body = build_request_body(&config, &[], &[], true);
        apply_max_tokens(&mut body, &config, 128);
        assert_eq!(body["max_completion_tokens"], 128);
    }
}
