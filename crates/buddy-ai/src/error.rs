//! Bounded provider errors.
//!
//! Every message that can be derived from remote input (HTTP bodies, SSE
//! payloads, JSON snippets) is truncated before it reaches an error value.
//! That keeps a broken or hostile endpoint from filling logs or the UI.

use std::fmt;

/// Maximum number of characters carried in a provider error message.
pub const MAX_ERROR_CHARS: usize = 600;

/// Maximum number of bytes read from a non-success response body.
pub const MAX_ERROR_BODY_BYTES: usize = 4096;

/// Truncate text on a character boundary, appending a marker when shortened.
pub fn truncate(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (count, ch) in text.chars().enumerate() {
        if count == max_chars {
            out.push_str("... [truncated]");
            return out;
        }
        out.push(ch);
    }
    out
}

/// Short, bounded rendering of an arbitrary JSON value for diagnostics.
pub fn snippet(value: &serde_json::Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string());
    truncate(&text, MAX_ERROR_CHARS)
}

/// Errors produced by the provider layer.
#[derive(Debug)]
pub enum ProviderError {
    /// The caller cancelled the request or the stream.
    Cancelled,
    /// Local configuration is unusable (missing endpoint, model, key, ...).
    Config(String),
    /// Transport-level failure. The text is already bounded.
    Http(String),
    /// Non-success HTTP status.
    Status { status: u16, body: String },
    /// The peer sent data that does not match the chat-completions protocol.
    Protocol(String),
    /// The peer reported an error inside a 200 response.
    Upstream(String),
}

impl ProviderError {
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(truncate(&message.into(), MAX_ERROR_CHARS))
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(truncate(&message.into(), MAX_ERROR_CHARS))
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::Upstream(truncate(&message.into(), MAX_ERROR_CHARS))
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("provider request cancelled"),
            Self::Config(message) => write!(f, "provider configuration error: {message}"),
            Self::Http(message) => write!(f, "provider request failed: {message}"),
            Self::Status { status, body } if body.is_empty() => {
                write!(f, "provider returned HTTP {status}")
            }
            Self::Status { status, body } => {
                write!(f, "provider returned HTTP {status}: {body}")
            }
            Self::Protocol(message) => write!(f, "provider protocol error: {message}"),
            Self::Upstream(message) => write!(f, "provider error: {message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// True when an error (or any error in its chain) is a cancellation.
pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ProviderError>()
            .map(ProviderError::is_cancelled)
            .unwrap_or(false)
    })
}

/// Extract a human-readable message from an OpenAI-style error payload.
pub fn upstream_message(error: &serde_json::Value) -> String {
    if let Some(text) = error.as_str() {
        return truncate(text, MAX_ERROR_CHARS);
    }
    for pointer in ["/message", "/error/message", "/detail"] {
        if let Some(text) = error.pointer(pointer).and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                return truncate(text, MAX_ERROR_CHARS);
            }
        }
    }
    let rendered = snippet(error);
    if rendered.trim().is_empty() || rendered == "null" {
        "endpoint reported an unspecified error".to_string()
    } else {
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_bounded_and_utf8_safe() {
        let text = "é".repeat(MAX_ERROR_CHARS + 100);
        let out = truncate(&text, 10);
        assert_eq!(out.chars().filter(|c| *c == 'é').count(), 10);
        assert!(out.ends_with("... [truncated]"));
        assert!(out.len() < text.len());
    }

    #[test]
    fn cancellation_is_detectable_in_the_chain() {
        let error = anyhow::Error::from(ProviderError::Cancelled);
        assert!(is_cancelled(&error));
        let wrapped = error.context("while streaming");
        assert!(is_cancelled(&wrapped));

        let other = anyhow::Error::from(ProviderError::protocol("bad"));
        assert!(!is_cancelled(&other));
    }

    #[test]
    fn upstream_messages_are_bounded() {
        let value = serde_json::json!({"message": "x".repeat(5000)});
        let message = upstream_message(&value);
        assert!(message.chars().count() <= MAX_ERROR_CHARS + 20);
    }
}
