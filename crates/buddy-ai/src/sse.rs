//! Incremental Server-Sent Events parser.
//!
//! Pure, byte-oriented, and independent of HTTP. Feed it whatever chunks arrive
//! from the socket: lines, CRLF pairs, and multi-byte UTF-8 characters may all
//! be split arbitrarily. Events are only decoded once a full line is available,
//! so a split character can never produce lossy text.
//!
//! Supported per the SSE specification: `data`, `event`, `id`, `retry`,
//! comments (`:`), LF/CRLF/CR line endings, and multi-line `data` joined with
//! newlines. Parsing is bounded so a malicious peer cannot grow the buffer
//! without limit.

use std::fmt;

/// Maximum bytes buffered for a single line before the stream is rejected.
pub const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;

/// Maximum bytes accumulated for a single event's data.
pub const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// A dispatched SSE event. `event` defaults to `"message"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Bounded SSE framing error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseError {
    message: String,
}

impl SseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: crate::error::truncate(&message.into(), 300),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SSE parse error: ")?;
        f.write_str(&self.message)
    }
}

impl std::error::Error for SseError {}

/// Incremental SSE parser state.
#[derive(Debug)]
pub struct SseParser {
    buffer: Vec<u8>,
    data: String,
    event: Option<String>,
    id: Option<String>,
    have_data: bool,
    max_line_bytes: usize,
    max_event_bytes: usize,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            data: String::new(),
            event: None,
            id: None,
            have_data: false,
            max_line_bytes: MAX_SSE_LINE_BYTES,
            max_event_bytes: MAX_SSE_EVENT_BYTES,
        }
    }

    /// Override the framing limits. Mainly useful for tests.
    pub fn with_limits(max_line_bytes: usize, max_event_bytes: usize) -> Self {
        Self {
            max_line_bytes: max_line_bytes.max(1),
            max_event_bytes: max_event_bytes.max(1),
            ..Self::new()
        }
    }

    /// Feed bytes received from the network. Returns every event completed by
    /// this chunk, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut consumed = 0usize;
        while let Some((line_len, next_len)) = find_line(&self.buffer[consumed..]) {
            if line_len > self.max_line_bytes {
                return Err(SseError::new(format!(
                    "line exceeds {} bytes",
                    self.max_line_bytes
                )));
            }
            let line = self.buffer[consumed..consumed + line_len].to_vec();
            consumed += next_len;
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        if self.buffer.len() > self.max_line_bytes {
            return Err(SseError::new(format!(
                "unterminated line exceeds {} bytes",
                self.max_line_bytes
            )));
        }
        Ok(events)
    }

    /// Flush the final line and any pending event at end of stream.
    ///
    /// Many OpenAI-compatible servers close the connection without a trailing
    /// blank line, so this must be called on EOF.
    pub fn finish(&mut self) -> Result<Option<SseEvent>, SseError> {
        if !self.buffer.is_empty() {
            let mut line = std::mem::take(&mut self.buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > self.max_line_bytes {
                return Err(SseError::new(format!(
                    "line exceeds {} bytes",
                    self.max_line_bytes
                )));
            }
            if let Some(event) = self.process_line(&line)? {
                return Ok(Some(event));
            }
        }
        Ok(self.dispatch())
    }

    /// Number of bytes currently buffered but not yet formed into a line.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<SseEvent>, SseError> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line[0] == b':' {
            return Ok(None);
        }
        let text =
            std::str::from_utf8(line).map_err(|_| SseError::new("SSE line is not valid UTF-8"))?;
        let (field, raw_value) = match text.find(':') {
            Some(index) => (&text[..index], &text[index + 1..]),
            None => (text, ""),
        };
        let value = raw_value.strip_prefix(' ').unwrap_or(raw_value);

        match field {
            "data" => {
                if self.data.len() + value.len() + 1 > self.max_event_bytes {
                    return Err(SseError::new(format!(
                        "event data exceeds {} bytes",
                        self.max_event_bytes
                    )));
                }
                self.data.push_str(value);
                self.data.push('\n');
                self.have_data = true;
            }
            "event" => {
                self.event = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "id" => {
                if !value.contains('\0') {
                    self.id = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
            }
            "retry" => {
                // Retry is irrelevant for a single request, but consuming the
                // line avoids treating it as data.
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.have_data {
            self.event = None;
            return None;
        }
        // Every data line appended a newline; the final one is not part of the
        // value.
        self.data.pop();
        let data = std::mem::take(&mut self.data);
        let event = self.event.take().unwrap_or_else(|| "message".to_string());
        let id = self.id.take();
        self.have_data = false;
        Some(SseEvent { event, data, id })
    }
}

/// Locate the next line and return `(line_len, consumed)`, excluding the line
/// terminator from the length.
///
/// A trailing `\r` is not treated as a line ending yet because it may be the
/// first half of a CRLF split across chunks.
fn find_line(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < buffer.len() {
        match buffer[index] {
            b'\n' => return Some((index, index + 1)),
            b'\r' => {
                if index + 1 < buffer.len() {
                    if buffer[index + 1] == b'\n' {
                        return Some((index, index + 2));
                    }
                    return Some((index, index + 1));
                }
                return None;
            }
            _ => index += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(parser: &mut SseParser, chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.push(chunk).expect("parse chunk"));
        }
        if let Some(event) = parser.finish().expect("finish") {
            events.push(event);
        }
        events
    }

    #[test]
    fn parses_basic_events_and_done() {
        let mut parser = SseParser::new();
        let events = collect(&mut parser, &[b"data: {\"a\":1}\n\ndata: [DONE]\n\n"]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "message");
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].data, "[DONE]");
    }

    #[test]
    fn handles_byte_at_a_time_and_crlf() {
        let wire = b"event: chunk\r\ndata: hel\r\ndata: lo\r\n\r\n";
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for byte in wire {
            events.extend(parser.push(std::slice::from_ref(byte)).expect("byte"));
        }
        events.extend(parser.finish().expect("finish"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "chunk");
        assert_eq!(events[0].data, "hel\nlo");
    }

    #[test]
    fn handles_lone_cr_and_missing_trailing_newline() {
        let mut parser = SseParser::new();
        let mut events = parser.push(b"data: one\r").expect("push");
        assert!(events.is_empty(), "trailing CR must wait for CRLF decision");
        events.extend(parser.push(b"\rdata: two\r\r").expect("push"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "one");
        // The pending `data: two` event is flushed at EOF.
        let final_event = parser.finish().expect("finish").expect("pending event");
        assert_eq!(final_event.data, "two");
    }

    #[test]
    fn preserves_split_utf8() {
        let wire = "data: héllo wörld\n\n".as_bytes();
        // Split inside the two-byte `é` and `ö`.
        let split_a = wire
            .iter()
            .position(|b| *b == 0xC3)
            .expect("utf8 lead byte");
        let mut parser = SseParser::new();
        let mut events = parser.push(&wire[..split_a + 1]).expect("first");
        assert!(events.is_empty());
        events.extend(parser.push(&wire[split_a + 1..]).expect("second"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "héllo wörld");
    }

    #[test]
    fn ignores_comments_and_unknown_fields() {
        let mut parser = SseParser::new();
        let events = collect(
            &mut parser,
            &[b": keepalive\nretry: 1000\nfoo: bar\ndata:value\n\n"],
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "value");
    }

    #[test]
    fn blank_data_line_is_an_empty_event() {
        let mut parser = SseParser::new();
        let events = collect(&mut parser, &[b"data:\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn rejects_oversized_line() {
        let mut parser = SseParser::with_limits(8, 100);
        let result = parser.push(b"data: 0123456789\n\n");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_oversized_event() {
        let mut parser = SseParser::with_limits(100, 8);
        let result = parser.push(b"data: 0123456789\n\n");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_utf8_line() {
        let mut parser = SseParser::new();
        let result = parser.push(b"data: \xff\n\n");
        assert!(result.is_err());
    }

    #[test]
    fn id_is_attached_to_the_event() {
        let mut parser = SseParser::new();
        let events = collect(&mut parser, &[b"id: 7\ndata: x\n\n"]);
        assert_eq!(events[0].id.as_deref(), Some("7"));
    }
}
