//! OpenAI-compatible chat-completions provider for Drops.
//!
//! The crate is split into pure pieces and a thin HTTP layer:
//!
//! - [`request`] builds the wire request.
//! - [`sse`] incrementally frames Server-Sent Events from arbitrary byte
//!   fragments.
//! - [`stream`] accumulates text, reasoning, tool calls, and usage.
//! - [`complete`] performs one streaming HTTP request and returns the finished
//!   assistant message.
//!
//! Tool arguments are never parsed while streaming. Each call is assembled by
//! `index`, ids are generated when a server omits them, and the final
//! `arguments` string is handed to the agent layer, which reports malformed
//! JSON back to the model instead of failing the turn.

mod config;
mod error;
mod http;
mod request;
mod sse;
mod stream;

pub use config::ProviderConfig;
pub use error::{
    is_cancelled, snippet, truncate, upstream_message, ProviderError, MAX_ERROR_BODY_BYTES,
    MAX_ERROR_CHARS,
};
pub use http::complete;
pub use request::{apply_max_tokens, build_request_body, endpoint};
pub use sse::{SseError, SseEvent, SseParser, MAX_SSE_EVENT_BYTES, MAX_SSE_LINE_BYTES};
pub use stream::{
    StreamAccumulator, MAX_CONTENT_BYTES, MAX_REASONING_BYTES, MAX_TOOL_ARGUMENT_BYTES,
    MAX_TOOL_CALLS,
};

use serde_json::Value;

/// An OpenAI wire message.
pub type Message = Value;
/// An OpenAI tool definition.
pub type ToolDefinition = Value;
/// An OpenAI assistant tool-call object.
pub type ToolCall = Value;

/// Streamed provider output.
///
/// `Text` and `Thinking` are emitted as fragments arrive. `ToolCall` and
/// `Usage` are emitted once the response is complete: tool calls in index
/// order, then usage if the server supplied it.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    Text(String),
    Thinking(String),
    ToolCall(Value),
    Usage(Value),
}
