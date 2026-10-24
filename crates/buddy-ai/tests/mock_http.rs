//! End-to-end tests against a hand-rolled mock HTTP server.
//!
//! No extra test dependencies: the server speaks just enough HTTP/1.1 to accept
//! one request, capture it, and replay a scripted SSE body in fragmented writes.

use std::collections::HashMap;
use std::time::Duration;

use buddy_ai::{
    build_request_body, complete, endpoint, is_cancelled, list_models, parse_models,
    ProviderConfig, ProviderError, ProviderEvent, StreamAccumulator,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

struct MockResponse {
    status_line: String,
    content_type: String,
    chunks: Vec<(Duration, Vec<u8>)>,
}

impl MockResponse {
    fn sse(body: Vec<u8>) -> Self {
        Self {
            status_line: "200 OK".to_string(),
            content_type: "text/event-stream".to_string(),
            chunks: vec![(Duration::ZERO, body)],
        }
    }

    fn json(body: &str) -> Self {
        Self {
            status_line: "200 OK".to_string(),
            content_type: "application/json".to_string(),
            chunks: vec![(Duration::ZERO, body.as_bytes().to_vec())],
        }
    }

    fn sse_fragmented(body: &[u8], size: usize) -> Self {
        let chunks = body
            .chunks(size)
            .map(|chunk| (Duration::from_millis(1), chunk.to_vec()))
            .collect();
        Self {
            status_line: "200 OK".to_string(),
            content_type: "text/event-stream; charset=utf-8".to_string(),
            chunks,
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<CapturedRequest> {
    let mut buffer = Vec::new();
    let mut scratch = [0u8; 4096];
    let (header_end, content_length) = loop {
        let read = socket.read(&mut scratch).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&scratch[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..position]);
            let content_length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            break (position + 4, content_length);
        }
        if buffer.len() > 1024 * 1024 {
            return None;
        }
    };

    while buffer.len() < header_end + content_length {
        let read = socket.read(&mut scratch).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&scratch[..read]);
    }

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let body_bytes = &buffer[header_end..buffer.len().min(header_end + content_length)];
    let body = serde_json::from_slice(body_bytes).unwrap_or(Value::Null);

    Some(CapturedRequest {
        path,
        headers,
        body,
    })
}

async fn spawn_mock(
    response: MockResponse,
) -> (
    String,
    oneshot::Receiver<CapturedRequest>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let address = listener.local_addr().expect("address");
    let (sender, receiver) = oneshot::channel();

    let handle = tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        if let Some(request) = read_request(&mut socket).await {
            let _ = sender.send(request);
        }
        let header = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
            response.status_line, response.content_type
        );
        if socket.write_all(header.as_bytes()).await.is_err() {
            return;
        }
        for (delay, chunk) in response.chunks {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if socket.write_all(&chunk).await.is_err() {
                return;
            }
            let _ = socket.flush().await;
        }
        let _ = socket.shutdown().await;
    });

    (format!("http://{address}/v1"), receiver, handle)
}

fn config(base_url: String, api_key_env: &str) -> ProviderConfig {
    ProviderConfig {
        base_url,
        model: "mock-model".to_string(),
        api_key_env: api_key_env.to_string(),
        vision: false,
        tools: true,
        max_tokens_param: "max_tokens".to_string(),
        ..ProviderConfig::default()
    }
}

fn sse(data: &str) -> String {
    format!("data: {data}\r\n\r\n")
}

fn drain(receiver: &mut mpsc::UnboundedReceiver<ProviderEvent>) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

fn text_of(events: &[ProviderEvent]) -> (String, String) {
    let mut text = String::new();
    let mut thinking = String::new();
    for event in events {
        match event {
            ProviderEvent::Text(fragment) => text.push_str(fragment),
            ProviderEvent::Thinking(fragment) => thinking.push_str(fragment),
            _ => {}
        }
    }
    (text, thinking)
}

#[tokio::test]
async fn streams_fragmented_text_reasoning_tools_and_usage() {
    const KEY_ENV: &str = "BUDDY_AI_TEST_KEY_STREAM";
    std::env::set_var(KEY_ENV, "test-secret");

    let mut body = String::new();
    body.push_str(&sse(
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#,
    ));
    body.push_str(&sse(
        r#"{"choices":[{"index":0,"delta":{"content":"lo","reasoning_content":"hé"}}]}"#,
    ));
    body.push_str(&sse(
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"echo","arguments":"{\"te"}},{"index":1,"type":"function","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
    ));
    body.push_str(&sse(
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"th\":\"/tmp\"}"}},{"index":0,"function":{"arguments":"xt\":\"hi\"}"}}]}}]}"#,
    ));
    body.push_str(&sse(
        r#"{"choices":[{"index":0,"finish_reason":"tool_calls","delta":{}}]}"#,
    ));
    body.push_str(&sse(
        r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}}"#,
    ));
    body.push_str("data: [DONE]\r\n\r\n");

    let (base_url, request, server) =
        spawn_mock(MockResponse::sse_fragmented(body.as_bytes(), 7)).await;

    let tools = vec![json!({
        "type": "function",
        "function": {"name": "echo", "description": "echo", "parameters": {"type": "object"}}
    })];
    let messages = vec![json!({"role": "user", "content": "say hi"})];
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    let message = complete(
        &config(base_url, KEY_ENV),
        &messages,
        &tools,
        sender,
        cancel,
    )
    .await
    .expect("complete");

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "Hello");
    let calls = message["tool_calls"].as_array().expect("tool calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_abc");
    assert_eq!(calls[0]["function"]["name"], "echo");
    assert_eq!(calls[0]["function"]["arguments"], r#"{"text":"hi"}"#);
    assert_eq!(calls[1]["function"]["name"], "read");
    assert_eq!(calls[1]["function"]["arguments"], r#"{"path":"/tmp"}"#);
    let generated_id = calls[1]["id"].as_str().expect("generated id");
    assert!(!generated_id.is_empty(), "missing ids are generated");

    let events = drain(&mut receiver);
    let (text, thinking) = text_of(&events);
    assert_eq!(text, "Hello");
    assert_eq!(thinking, "hé");
    let tool_events = events
        .iter()
        .filter(|event| matches!(event, ProviderEvent::ToolCall(_)))
        .count();
    assert_eq!(tool_events, 2);
    assert!(
        matches!(events.last(), Some(ProviderEvent::Usage(usage)) if usage["total_tokens"] == 18),
        "usage is emitted last: {events:?}"
    );

    let captured = request.await.expect("captured request");
    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer test-secret")
    );
    assert_eq!(captured.body["model"], "mock-model");
    assert_eq!(captured.body["stream"], true);
    assert_eq!(captured.body["stream_options"]["include_usage"], true);
    assert_eq!(captured.body["messages"][0]["content"], "say hi");
    assert_eq!(captured.body["tools"][0]["function"]["name"], "echo");

    std::env::remove_var(KEY_ENV);
    server.abort();
}

#[tokio::test]
async fn tolerates_eof_without_done_and_trailing_partial_line() {
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"}}]}"
    );
    let (base_url, _request, server) =
        spawn_mock(MockResponse::sse(body.as_bytes().to_vec())).await;

    let (sender, mut receiver) = mpsc::unbounded_channel();
    let message = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect("complete");
    assert_eq!(message["content"], "ok!");
    assert!(message.get("tool_calls").is_none());
    let events = drain(&mut receiver);
    assert_eq!(text_of(&events).0, "ok!");
    server.abort();
}

#[tokio::test]
async fn sends_keyless_requests_when_no_env_is_configured() {
    let body = sse(r#"{"choices":[{"index":0,"delta":{"content":"x"}}]}"#);
    let (base_url, request, server) = spawn_mock(MockResponse::sse(body.into_bytes())).await;

    let (sender, _receiver) = mpsc::unbounded_channel();
    complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect("complete");

    let captured = request.await.expect("request");
    assert!(!captured.headers.contains_key("authorization"));
    server.abort();
}

#[tokio::test]
async fn missing_configured_key_env_is_a_config_error() {
    const KEY_ENV: &str = "BUDDY_AI_TEST_KEY_ABSENT_9F3";
    std::env::remove_var(KEY_ENV);
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &config("http://127.0.0.1:9/v1".to_string(), KEY_ENV),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("missing key");
    assert!(error.to_string().contains(KEY_ENV));
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::Config(_))
    ));
}

#[tokio::test]
async fn cancellation_before_the_request_does_not_touch_the_network() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &config("http://127.0.0.1:9/v1".to_string(), ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        cancel,
    )
    .await
    .expect_err("cancelled");
    assert!(is_cancelled(&error), "error: {error}");
}

#[tokio::test]
async fn cancels_mid_stream_promptly() {
    let first = sse(r#"{"choices":[{"index":0,"delta":{"content":"partial"}}]}"#);
    let response = MockResponse {
        status_line: "200 OK".to_string(),
        content_type: "text/event-stream".to_string(),
        chunks: vec![
            (Duration::ZERO, first.into_bytes()),
            (Duration::from_secs(10), b"data: [DONE]\r\n\r\n".to_vec()),
        ],
    };
    let (base_url, _request, server) = spawn_mock(response).await;

    let (sender, mut receiver) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_for_task.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        complete(
            &config(base_url, ""),
            &[json!({"role":"user","content":"x"})],
            &[],
            sender,
            cancel,
        ),
    )
    .await
    .expect("cancellation must not wait for the server");
    let error = result.expect_err("cancelled");
    assert!(is_cancelled(&error), "error: {error}");
    assert!(
        !drain(&mut receiver).is_empty(),
        "text before cancellation was streamed"
    );
    server.abort();
}

#[tokio::test]
async fn status_errors_are_bounded() {
    let response = MockResponse {
        status_line: "500 Internal Server Error".to_string(),
        content_type: "application/json".to_string(),
        chunks: vec![(
            Duration::ZERO,
            format!("{{\"error\":\"{}\"}}", "x".repeat(100_000)).into_bytes(),
        )],
    };
    let (base_url, _request, server) = spawn_mock(response).await;

    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("status error");

    let provider_error = error
        .downcast_ref::<ProviderError>()
        .expect("provider error type");
    assert!(matches!(
        provider_error,
        ProviderError::Status { status: 500, .. }
    ));
    let message = error.to_string();
    assert!(
        message.len() < 1500,
        "error must stay bounded: {}",
        message.len()
    );
    assert!(!message.contains("[truncated]") || message.len() < 1500);
    server.abort();
}

#[tokio::test]
async fn accepts_non_streaming_json_responses() {
    let response = MockResponse {
        status_line: "200 OK".to_string(),
        content_type: "application/json".to_string(),
        chunks: vec![(
            Duration::ZERO,
            br#"{"choices":[{"index":0,"message":{"role":"assistant","content":"json","tool_calls":[{"id":"call_j","type":"function","function":{"name":"echo","arguments":"{\"a\":1}"}}]}}],"usage":{"total_tokens":9}}"#.to_vec(),
        )],
    };
    let (base_url, request, server) = spawn_mock(response).await;

    let (sender, mut receiver) = mpsc::unbounded_channel();
    let message = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect("complete");
    assert_eq!(message["content"], "json");
    assert_eq!(message["tool_calls"][0]["id"], "call_j");
    let events = drain(&mut receiver);
    assert!(events
        .iter()
        .any(|event| matches!(event, ProviderEvent::Usage(_))));
    let captured = request.await.expect("request");
    assert_eq!(captured.body["stream"], true);
    server.abort();
}

#[tokio::test]
async fn upstream_error_payload_fails_with_bounded_message() {
    let body = sse(&format!(
        r#"{{"error":{{"message":"{}"}}}}"#,
        "nope".repeat(2000)
    ));
    let (base_url, _request, server) = spawn_mock(MockResponse::sse(body.into_bytes())).await;

    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("upstream error");
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::Upstream(_))
    ));
    assert!(error.to_string().len() < 1500);
    server.abort();
}

#[tokio::test]
async fn sniffs_json_when_content_type_is_missing() {
    let response = MockResponse {
        status_line: "200 OK".to_string(),
        content_type: String::new(),
        chunks: vec![(
            Duration::ZERO,
            br#"{"choices":[{"index":0,"message":{"role":"assistant","content":"sniffed"}}]}"#
                .to_vec(),
        )],
    };
    let (base_url, _request, server) = spawn_mock(response).await;

    let (sender, _receiver) = mpsc::unbounded_channel();
    let message = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect("sniffed JSON");
    assert_eq!(message["content"], "sniffed");
    server.abort();
}

#[tokio::test]
async fn choice_less_stream_is_a_protocol_error() {
    let body = sse(r#"{"choices":[],"usage":{"total_tokens":1}}"#);
    let (base_url, _request, server) = spawn_mock(MockResponse::sse(body.into_bytes())).await;
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("no choices");
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::Protocol(_))
    ));
    server.abort();
}

#[tokio::test]
async fn empty_stream_is_a_protocol_error() {
    let response = MockResponse {
        status_line: "200 OK".to_string(),
        content_type: "text/event-stream".to_string(),
        chunks: vec![(Duration::ZERO, b"data: [DONE]\r\n\r\n".to_vec())],
    };
    let (base_url, _request, server) = spawn_mock(response).await;
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &config(base_url, ""),
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("empty stream");
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::Protocol(_))
    ));
    server.abort();
}

#[test]
fn request_helpers_are_available_as_pure_functions() {
    let config = config("http://localhost:11434/v1".to_string(), "");
    let body = build_request_body(&config, &[json!({"role":"user","content":"x"})], &[], true);
    assert_eq!(
        endpoint(&config.base_url),
        "http://localhost:11434/v1/chat/completions"
    );
    assert_eq!(body["stream_options"]["include_usage"], true);

    let mut accumulator = StreamAccumulator::new();
    accumulator
        .apply(&json!({"choices":[{"index":0,"delta":{"content":"x"}}]}))
        .expect("chunk");
    assert_eq!(accumulator.content(), "x");
}

#[test]
fn parse_models_accepts_common_shapes_and_is_stable() {
    let models = parse_models(&json!({
        "object": "list",
        "data": [
            {"id": "kimi-k3", "object": "model"},
            {"id": "deepseek-v4.1-flash"},
            {"id": "  kimi-k3  "},
            {"id": ""},
            {"object": "model"}
        ]
    }));
    assert_eq!(models, vec!["deepseek-v4.1-flash", "kimi-k3"]);
    assert_eq!(parse_models(&json!(["m2", "m1", "m2"])), vec!["m1", "m2"]);
    assert!(parse_models(&json!({"data": "nope"})).is_empty());
}

#[tokio::test]
async fn stored_key_takes_precedence_over_the_environment() {
    const KEY_ENV: &str = "BUDDY_AI_TEST_KEY_UNSET_PRECEDENCE";
    std::env::remove_var(KEY_ENV);
    let body = sse(r#"{"choices":[{"index":0,"delta":{"content":"x"}}]}"#);
    let (base_url, request, server) = spawn_mock(MockResponse::sse(body.into_bytes())).await;
    let mut provider = config(base_url, KEY_ENV);
    provider.api_key = Some("stored-secret".into());

    let (sender, _receiver) = mpsc::unbounded_channel();
    complete(
        &provider,
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect("complete");

    let captured = request.await.expect("request");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer stored-secret")
    );
    server.abort();
}

#[tokio::test]
async fn lists_models_with_the_stored_key() {
    let (base_url, request, server) = spawn_mock(MockResponse::json(
        r#"{"object":"list","data":[{"id":"z-model"},{"id":"a-model"}]}"#,
    ))
    .await;
    let mut provider = config(base_url, "");
    provider.api_key = Some("stored-secret".into());

    let models = list_models(&provider).await.expect("models");
    assert_eq!(models, vec!["a-model", "z-model"]);

    let captured = request.await.expect("request");
    assert_eq!(captured.path, "/v1/models");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer stored-secret")
    );
    server.abort();
}

#[tokio::test]
async fn model_list_errors_are_reported() {
    let (base_url, _request, server) = spawn_mock(MockResponse {
        status_line: "401 Unauthorized".to_string(),
        content_type: "application/json".to_string(),
        chunks: vec![(
            Duration::ZERO,
            br#"{"error":{"message":"invalid api key"}}"#.to_vec(),
        )],
    })
    .await;
    let error = list_models(&config(base_url, ""))
        .await
        .expect_err("unauthorized");
    assert!(error.to_string().contains("401"));
    server.abort();
}

#[tokio::test]
async fn upstream_errors_are_scrubbed_of_the_configured_key() {
    let (base_url, _request, server) = spawn_mock(MockResponse {
        status_line: "401 Unauthorized".to_string(),
        content_type: "application/json".to_string(),
        chunks: vec![(
            Duration::ZERO,
            br#"{"error":{"message":"Incorrect API key provided: stored-secret"}}"#.to_vec(),
        )],
    })
    .await;
    let mut provider = config(base_url, "");
    provider.api_key = Some("stored-secret".into());
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &provider,
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("401");
    assert!(!error.to_string().contains("stored-secret"));
    assert!(error.to_string().contains("<redacted>"));
    server.abort();
}

#[tokio::test]
async fn opencode_go_sends_its_session_header_and_identifies_the_client() {
    let body = sse(r#"{"choices":[{"index":0,"delta":{"content":"x"}}]}"#);
    let (base_url, request, server) = spawn_mock(MockResponse::sse(body.into_bytes())).await;
    let mut provider = config(base_url, "");
    provider.kind = buddy_ai::ProviderKind::OpencodeGo;
    provider.api_key = Some("stored-secret".into());
    provider.session_id = Some("drop-42".into());

    let (sender, _receiver) = mpsc::unbounded_channel();
    complete(
        &provider,
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect("complete");

    let captured = request.await.expect("request");
    assert_eq!(
        captured
            .headers
            .get("x-opencode-session")
            .map(String::as_str),
        Some("drop-42")
    );
    assert_eq!(
        captured.headers.get("user-agent").map(String::as_str),
        Some(concat!("drops/", env!("CARGO_PKG_VERSION")))
    );
    server.abort();
}

#[tokio::test]
async fn opencode_go_requires_a_session_id_before_the_request() {
    let mut provider = config("http://127.0.0.1:9/v1".to_string(), "");
    provider.kind = buddy_ai::ProviderKind::OpencodeGo;
    provider.api_key = Some("stored-secret".into());
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = complete(
        &provider,
        &[json!({"role":"user","content":"x"})],
        &[],
        sender,
        CancellationToken::new(),
    )
    .await
    .expect_err("missing session");
    assert!(error.to_string().contains("session id"));
    assert!(!error.to_string().contains("stored-secret"));
}
