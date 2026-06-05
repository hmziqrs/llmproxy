//! Buffered SSE parser for upstream provider streams.
//!
//! Provider streams can split a single SSE frame across multiple TCP chunks,
//! and some providers emit comments, event names, ids, or multi-line `data:`
//! fields. This module handles those edge cases so that route handlers and
//! adapters never need to split raw HTTP byte chunks with `str::lines()`.

use std::str;

use crate::error::ProviderError;

// ---------------------------------------------------------------------------
// SseFrame
// ---------------------------------------------------------------------------

/// A parsed SSE frame.
///
/// Emitted only after a blank line boundary or when [`SseFramer::finish`] is
/// called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// Optional `event:` field value.
    pub event: Option<String>,
    /// Optional `id:` field value.
    pub id: Option<String>,
    /// Collected `data:` field value(s), joined with `\n` for multi-line data.
    pub data: String,
}

// ---------------------------------------------------------------------------
// SseFramer
// ---------------------------------------------------------------------------

/// Buffered SSE frame parser.
///
/// Call [`SseFramer::push_chunk`] with raw bytes as they arrive from the
/// upstream. Call [`SseFramer::finish`] when the stream ends to emit any
/// trailing partial frame.
#[derive(Debug, Default)]
pub struct SseFramer {
    /// Buffered partial line data (valid UTF-8).
    buffer: String,
    /// Current frame being assembled.
    current_event: Option<String>,
    current_id: Option<String>,
    current_data_lines: Vec<String>,
}

impl SseFramer {
    /// Create a new empty framer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw byte chunk and return any completed frames.
    ///
    /// The chunk may contain partial lines. Completed frames are emitted only
    /// when a blank line boundary is encountered. Returns an error for invalid
    /// UTF-8.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ProviderError> {
        let text = str::from_utf8(chunk)?;
        self.buffer.push_str(text);
        self.drain_buffer()
    }

    /// Flush any remaining buffered data as a final frame.
    ///
    /// Call this when the upstream stream ends. If there is partial data in the
    /// buffer, it is emitted as a frame.
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, ProviderError> {
        // If there is anything left in the buffer, treat it as a line.
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            let line = line.trim_end_matches('\r');
            self.process_line(line);
        }
        let frame = self.take_current_frame();
        Ok(frame.map(|f| vec![f]).unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    /// Parse as many complete lines as possible from the buffer.
    fn drain_buffer(&mut self) -> Result<Vec<SseFrame>, ProviderError> {
        let mut frames = Vec::new();

        while let Some(nl_pos) = self.buffer.find('\n') {
            // Extract the line (trim trailing \r) and remove from buffer.
            let line = self.buffer[..nl_pos].trim_end_matches('\r').to_owned();
            self.buffer.drain(..nl_pos + 1);

            if line.is_empty() {
                // Blank line = frame boundary.
                if let Some(frame) = self.take_current_frame() {
                    frames.push(frame);
                }
            } else {
                self.process_line(&line);
            }
        }

        Ok(frames)
    }

    /// Process a single non-blank SSE line.
    fn process_line(&mut self, line: &str) {
        // Ignore comments.
        if line.starts_with(':') {
            return;
        }

        // Parse field:value or field: value (space after colon is optional).
        if let Some(colon_pos) = line.find(':') {
            let field = &line[..colon_pos];
            // Skip the optional single space after the colon.
            let value = &line[colon_pos + 1..];
            let value = value.strip_prefix(' ').unwrap_or(value);

            match field {
                "event" => {
                    self.current_event = Some(value.to_owned());
                }
                "id" => {
                    self.current_id = Some(value.to_owned());
                }
                "data" => {
                    self.current_data_lines.push(value.to_owned());
                }
                // Ignore unknown fields per spec.
                _ => {}
            }
        } else {
            // A field with no colon and no value (e.g. just "data" with no ":").
            // Per SSE spec, this is a field with an empty value.
            match line {
                "event" => {
                    self.current_event = Some(String::new());
                }
                "id" => {
                    self.current_id = Some(String::new());
                }
                "data" => {
                    self.current_data_lines.push(String::new());
                }
                _ => {}
            }
        }
    }

    /// Take the current accumulated frame if it has any data.
    fn take_current_frame(&mut self) -> Option<SseFrame> {
        let data_lines = std::mem::take(&mut self.current_data_lines);
        if data_lines.is_empty() && self.current_event.is_none() && self.current_id.is_none() {
            return None;
        }

        let data = data_lines.join("\n");
        Some(SseFrame {
            event: self.current_event.take(),
            id: self.current_id.take(),
            data,
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_framer_handles_partial_frames_split_across_chunks() {
        let mut framer = SseFramer::new();

        // First chunk has a partial frame.
        let frames = framer.push_chunk(b"data: hel").unwrap();
        assert!(frames.is_empty(), "No complete frame yet");

        let frames = framer.push_chunk(b"lo\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn sse_framer_handles_multi_line_data_fields() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"data: line1\ndata: line2\ndata: line3\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "line1\nline2\nline3");
    }

    #[test]
    fn sse_framer_handles_crlf() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"data: hello\r\n\r\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn sse_framer_ignores_comments() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b": this is a comment\ndata: hello\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn sse_framer_preserves_event_and_id() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"event: message_start\nid: 42\ndata: {\"type\":\"start\"}\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("message_start"));
        assert_eq!(frames[0].id.as_deref(), Some("42"));
        assert_eq!(frames[0].data, "{\"type\":\"start\"}");
    }

    #[test]
    fn sse_framer_emits_trailing_frame_from_finish() {
        let mut framer = SseFramer::new();

        // Push data without a trailing blank line.
        let frames = framer.push_chunk(b"data: partial").unwrap();
        assert!(frames.is_empty());

        // Finish should emit the trailing frame.
        let frames = framer.finish().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "partial");
    }

    #[test]
    fn sse_framer_respects_blank_line_frame_boundaries() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"data: first\n\ndata: second\n\n")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "first");
        assert_eq!(frames[1].data, "second");
    }

    #[test]
    fn sse_framer_preserves_done() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"data: {\"content\":\"hi\"}\n\ndata: [DONE]\n\n")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "{\"content\":\"hi\"}");
        assert_eq!(frames[1].data, "[DONE]");
    }

    #[test]
    fn sse_framer_rejects_invalid_utf8() {
        let mut framer = SseFramer::new();

        let result = framer.push_chunk(&[0xFF, 0xFE]);
        assert!(
            result.is_err(),
            "Expected error for invalid UTF-8"
        );
        match result.unwrap_err() {
            ProviderError::Utf8(_) => {}
            other => panic!("Expected ProviderError::Utf8, got: {:?}", other),
        }
    }

    #[test]
    fn sse_framer_multiple_frames_across_many_chunks() {
        let mut framer = SseFramer::new();
        let mut all_frames = Vec::new();

        all_frames.extend(framer.push_chunk(b"data: a").unwrap());
        all_frames.extend(framer.push_chunk(b"\n\nda").unwrap());
        all_frames.extend(framer.push_chunk(b"ta: b\n\nda").unwrap());
        all_frames.extend(framer.push_chunk(b"ta: c\n\n").unwrap());

        assert_eq!(all_frames.len(), 3);
        assert_eq!(all_frames[0].data, "a");
        assert_eq!(all_frames[1].data, "b");
        assert_eq!(all_frames[2].data, "c");
    }

    #[test]
    fn sse_framer_event_without_data() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"event: ping\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("ping"));
        assert_eq!(frames[0].data, "");
    }

    #[test]
    fn sse_framer_mixed_crlf_and_lf() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"data: first\r\n\r\ndata: second\n\n")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "first");
        assert_eq!(frames[1].data, "second");
    }

    #[test]
    fn sse_framer_id_and_event_reset_per_frame() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"event: msg1\nid: 1\ndata: hello\n\ndata: world\n\n")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("msg1"));
        assert_eq!(frames[0].id.as_deref(), Some("1"));
        assert_eq!(frames[0].data, "hello");
        // Second frame has no event or id.
        assert!(frames[1].event.is_none());
        assert!(frames[1].id.is_none());
        assert_eq!(frames[1].data, "world");
    }

    // -----------------------------------------------------------------------
    // Edge case tests
    // -----------------------------------------------------------------------

    #[test]
    fn sse_framer_empty_input_returns_no_frames() {
        let mut framer = SseFramer::new();

        let frames = framer.push_chunk(b"").unwrap();
        assert!(frames.is_empty(), "Empty chunk should produce no frames");

        let frames = framer.finish().unwrap();
        assert!(
            frames.is_empty(),
            "Finish on empty framer should produce no frames"
        );
    }

    #[test]
    fn sse_framer_only_comments_yields_no_frames() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b": comment one\n: comment two\n\n")
            .unwrap();
        assert!(
            frames.is_empty(),
            "Comments-only stream should produce no frames"
        );

        let frames = framer.finish().unwrap();
        assert!(
            frames.is_empty(),
            "Finish after comments-only stream should produce no frames"
        );
    }

    #[test]
    fn sse_framer_only_blank_lines_yields_no_frames() {
        let mut framer = SseFramer::new();

        let frames = framer.push_chunk(b"\n\n\n\n").unwrap();
        assert!(
            frames.is_empty(),
            "Blank lines without data fields should produce no frames"
        );

        let frames = framer.finish().unwrap();
        assert!(
            frames.is_empty(),
            "Finish after blank lines should produce no frames"
        );
    }

    #[test]
    fn sse_framer_data_field_empty_value() {
        let mut framer = SseFramer::new();

        let frames = framer.push_chunk(b"data:\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "");
    }

    #[test]
    fn sse_framer_finish_after_complete_stream_is_empty() {
        let mut framer = SseFramer::new();

        // Push a complete frame (terminated by blank line).
        let frames = framer.push_chunk(b"data: hello\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");

        // Calling finish() again should return empty.
        let frames = framer.finish().unwrap();
        assert!(
            frames.is_empty(),
            "Finish after fully drained stream should produce no frames"
        );
    }

    #[test]
    fn sse_framer_ignores_unknown_fields() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"retry: 5000\ndata: hello\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");
        // The retry field is silently ignored per SSE spec.
    }

    #[test]
    fn sse_framer_preserves_url_with_multiple_colons() {
        let mut framer = SseFramer::new();

        let frames = framer
            .push_chunk(b"data: http://example.com:8080/api\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "http://example.com:8080/api");
    }

    #[test]
    fn sse_framer_finish_trims_trailing_cr() {
        let mut framer = SseFramer::new();

        // Simulate a stream ending with 'data: hello\r' (no trailing \n).
        let frames = framer.push_chunk(b"data: hello\r").unwrap();
        assert!(frames.is_empty());

        let frames = framer.finish().unwrap();
        assert_eq!(frames.len(), 1);
        // The trailing \r must be trimmed, not preserved in the data.
        assert_eq!(frames[0].data, "hello");
    }

    // -----------------------------------------------------------------------
    // Source guard: sse.rs must not import forbidden types
    // -----------------------------------------------------------------------

    #[test]
    fn sse_source_no_forbidden_imports() {
        let source = include_str!("sse.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);
        assert!(
            !prod.contains("llm_proxy_protocol"),
            "sse.rs must not import llm_proxy_protocol"
        );
        assert!(
            !prod.contains("EndpointType"),
            "sse.rs must not mention EndpointType"
        );
        assert!(
            !prod.contains("OpenCodeClient"),
            "sse.rs must not mention OpenCodeClient"
        );
        assert!(
            !prod.contains("classify_endpoint"),
            "sse.rs must not mention classify_endpoint"
        );
        assert!(
            !prod.contains("is_anthropic_model"),
            "sse.rs must not mention provider model classification"
        );
        assert!(
            !prod.contains("is_gemini_model"),
            "sse.rs must not mention provider model classification"
        );
    }
}
