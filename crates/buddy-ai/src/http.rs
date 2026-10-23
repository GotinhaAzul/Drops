//! The streaming HTTP layer.
//!
//! Everything protocol-related lives in pure modules; this file only moves
//! bytes, enforces cancellation, and bounds failures.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::error::{truncate, ProviderError, MAX_ERROR_BODY_BYTES, MAX_ERROR_CHARS};
use crate::request::{build_request_body, endpoint};
use crate::sse::{SseEvent, SseParser};
use crate::stream::StreamAccumulator;
use crate::{ProviderConfig, ProviderEvent};

/// Maximum bytes accepted for a non-streaming JSON response.
pub const MAX_JSON_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // Streaming responses can legitimately run for minutes, so there is
            // no whole-request timeout; only connection establishment is bounded.
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn cancelled() -> anyhow::Error {
    ProviderError::Cancelled.into()
}

/// Perform one chat completion, streaming events and returning the finished
/// assistant wire message (`role`, `content`, and `tool_calls` when present).
///
/// Cancellation is checked before the request and while the body is read. A
/// cancelled call returns a [`ProviderError::Cancelled`] error; use
/// [`crate::is_cancelled`] to distinguish it.
///
/// The API key, when configured, is read from the named environment variable at
/// call time and only sent in the `Authorization` header.
pub async fn complete(
    config: &ProviderConfig,
    messages: &[Value],
    tools: &[Value],
    events: UnboundedSender<ProviderEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<Value> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    if config.base_url.trim().is_empty() {
        return Err(ProviderError::config("base_url is empty").into());
    }
    if config.model.trim().is_empty() {
        return Err(ProviderError::config("model is empty").into());
    }

    let url = endpoint(&config.base_url);
    let body = build_request_body(config, messages, tools, true);
    let mut request = client()
        .post(&url)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body);

    if !config.api_key_env.trim().is_empty() {
        let env_name = config.api_key_env.trim();
        match std::env::var(env_name) {
            Ok(key) if !key.trim().is_empty() => request = request.bearer_auth(key),
            Ok(_) => {}
            Err(std::env::VarError::NotPresent) => {
                return Err(ProviderError::config(format!(
                    "environment variable {env_name} is not set"
                ))
                .into());
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ProviderError::config(format!(
                    "environment variable {env_name} is not valid UTF-8"
                ))
                .into());
            }
        }
    }

    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(cancelled()),
        response = request.send() => response.map_err(|error| {
            ProviderError::Http(truncate(&error.to_string(), MAX_ERROR_CHARS))
        })?,
    };

    let status = response.status();
    if !status.is_success() {
        let body = read_bounded_bytes(response, MAX_ERROR_BODY_BYTES, &cancel).await?;
        let body = truncate(String::from_utf8_lossy(&body).trim(), MAX_ERROR_CHARS);
        return Err(ProviderError::Status {
            status: status.as_u16(),
            body,
        }
        .into());
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if content_type.contains("application/json") || content_type.contains("text/json") {
        return complete_json(response, events, cancel).await;
    }

    complete_sse(response, &events, &cancel).await
}

async fn complete_json(
    response: reqwest::Response,
    events: UnboundedSender<ProviderEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<Value> {
    let bytes = read_bounded_bytes(response, MAX_JSON_RESPONSE_BYTES, &cancel).await?;
    finish_json(&bytes, &events)
}

/// Apply one complete non-streaming completion object.
fn finish_json(bytes: &[u8], events: &UnboundedSender<ProviderEvent>) -> anyhow::Result<Value> {
    let chunk: Value = serde_json::from_slice(bytes).map_err(|error| {
        ProviderError::protocol(format!(
            "invalid JSON response: {error}; body: {}",
            truncate(&String::from_utf8_lossy(bytes), 300)
        ))
    })?;

    let mut accumulator = StreamAccumulator::new();
    for event in accumulator.apply(&chunk)? {
        let _ = events.send(event);
    }
    if !accumulator.saw_choice() {
        return Err(ProviderError::protocol("response contained no completion choices").into());
    }
    let (message, deferred) = accumulator.finalize();
    for event in deferred {
        let _ = events.send(event);
    }
    Ok(message)
}

async fn complete_sse(
    response: reqwest::Response,
    events: &UnboundedSender<ProviderEvent>,
    cancel: &CancellationToken,
) -> anyhow::Result<Value> {
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();
    let mut accumulator = StreamAccumulator::new();
    let mut saw_payload = false;
    let mut done = false;
    // Servers occasionally omit `Content-Type` entirely. Buffer just enough of
    // the body to tell SSE (`data:`, comments) from a plain JSON completion.
    let mut initial: Option<Vec<u8>> = Some(Vec::new());

    while !done {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            next = stream.next() => next,
        };
        let Some(next) = next else {
            break;
        };
        let bytes = next
            .map_err(|error| ProviderError::Http(truncate(&error.to_string(), MAX_ERROR_CHARS)))?;

        let feed = if initial.is_some() {
            enum Sniff {
                More,
                Json(Vec<u8>),
                Sse(Vec<u8>),
                Waste,
            }
            let action = {
                let buffer = initial.as_mut().expect("checked above");
                buffer.extend_from_slice(&bytes);
                match buffer.iter().find(|byte| !byte.is_ascii_whitespace()) {
                    None if buffer.len() <= 128 => Sniff::More,
                    None => Sniff::Waste,
                    Some(b'{') => Sniff::Json(std::mem::take(buffer)),
                    Some(_) => Sniff::Sse(std::mem::take(buffer)),
                }
            };
            match action {
                Sniff::More => continue,
                Sniff::Waste => {
                    return Err(ProviderError::protocol(
                        "response began with whitespace and no data",
                    )
                    .into());
                }
                Sniff::Json(mut bytes) => {
                    loop {
                        let next = tokio::select! {
                            _ = cancel.cancelled() => return Err(cancelled()),
                            next = stream.next() => next,
                        };
                        let Some(next) = next else {
                            break;
                        };
                        let chunk = next.map_err(|error| {
                            ProviderError::Http(truncate(&error.to_string(), MAX_ERROR_CHARS))
                        })?;
                        if bytes.len() + chunk.len() > MAX_JSON_RESPONSE_BYTES {
                            return Err(ProviderError::protocol(format!(
                                "JSON response exceeds {MAX_JSON_RESPONSE_BYTES} bytes"
                            ))
                            .into());
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    return finish_json(&bytes, events);
                }
                Sniff::Sse(bytes) => {
                    initial = None;
                    bytes
                }
            }
        } else {
            bytes.to_vec()
        };

        let outcome = feed_frame(&feed, &mut parser, &mut accumulator, events)?;
        saw_payload |= outcome.payload;
        done |= outcome.done;
    }

    if let Some(buffer) = initial.take() {
        if buffer.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let outcome = feed_frame(&buffer, &mut parser, &mut accumulator, events)?;
            saw_payload |= outcome.payload;
            done |= outcome.done;
        }
    }

    if !done {
        if let Some(event) = parser
            .finish()
            .map_err(|error| ProviderError::protocol(error.to_string()))?
        {
            let outcome = handle_event(event, &mut accumulator, events)?;
            saw_payload |= outcome.payload;
            done |= outcome.done;
        }
    }

    if !saw_payload {
        return Err(ProviderError::protocol(if done {
            "stream ended with [DONE] before any data"
        } else {
            "stream ended without any data events"
        })
        .into());
    }
    if !accumulator.saw_choice() {
        return Err(ProviderError::protocol("response contained no completion choices").into());
    }

    let (message, deferred) = accumulator.finalize();
    for event in deferred {
        let _ = events.send(event);
    }
    Ok(message)
}

/// Frame one chunk and dispatch all completed SSE events.
fn feed_frame(
    bytes: &[u8],
    parser: &mut SseParser,
    accumulator: &mut StreamAccumulator,
    events: &UnboundedSender<ProviderEvent>,
) -> Result<EventOutcome, ProviderError> {
    let mut outcome = EventOutcome {
        done: false,
        payload: false,
    };
    let framed = parser
        .push(bytes)
        .map_err(|error| ProviderError::protocol(error.to_string()))?;
    for event in framed {
        let next = handle_event(event, accumulator, events)?;
        outcome.done |= next.done;
        outcome.payload |= next.payload;
    }
    Ok(outcome)
}

struct EventOutcome {
    done: bool,
    payload: bool,
}

fn handle_event(
    event: SseEvent,
    accumulator: &mut StreamAccumulator,
    events: &UnboundedSender<ProviderEvent>,
) -> Result<EventOutcome, ProviderError> {
    let data = event.data.trim();
    if data.is_empty() {
        return Ok(EventOutcome {
            done: false,
            payload: false,
        });
    }
    if data == "[DONE]" {
        return Ok(EventOutcome {
            done: true,
            payload: false,
        });
    }
    if event.event.eq_ignore_ascii_case("error") {
        let message = serde_json::from_str::<Value>(data)
            .map(|value| crate::error::upstream_message(&value))
            .unwrap_or_else(|_| truncate(data, MAX_ERROR_CHARS));
        return Err(ProviderError::upstream(message));
    }

    let chunk: Value = serde_json::from_str(data).map_err(|error| {
        ProviderError::protocol(format!(
            "invalid JSON in SSE data: {error}; data: {}",
            truncate(data, 300)
        ))
    })?;
    for event in accumulator.apply(&chunk)? {
        let _ = events.send(event);
    }
    Ok(EventOutcome {
        done: false,
        payload: true,
    })
}

/// Read at most `limit` bytes, stopping early once the limit is reached.
async fn read_bounded_bytes(
    response: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while buffer.len() < limit {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            next = stream.next() => next,
        };
        let Some(next) = next else {
            break;
        };
        let bytes = next
            .map_err(|error| ProviderError::Http(truncate(&error.to_string(), MAX_ERROR_CHARS)))?;
        let remaining = limit.saturating_sub(buffer.len());
        if bytes.len() > remaining {
            buffer.extend_from_slice(&bytes[..remaining]);
            break;
        }
        buffer.extend_from_slice(&bytes);
    }
    Ok(buffer)
}
