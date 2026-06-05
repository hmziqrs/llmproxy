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
    /// Buffered raw bytes. Stores partial UTF-8 sequences that arrived split
    /// across TCP chunks so that multi-byte characters (CJK, emoji) are not
    /// rejected by an intermediate `str::from_utf8` call.
    buffer: Vec<u8>,
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
    /// The chunk may contain partial lines or partial UTF-8 sequences.
    /// Completed frames are emitted only when a blank line boundary is
    /// encountered. Returns an error if the accumulated buffer (after appending
    /// the chunk) contains invalid UTF-8 that cannot be explained by a partial
    /// multi-byte sequence at the tail.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ProviderError> {
        self.buffer.extend_from_slice(chunk);
        self.drain_buffer()
    }

    /// Flush any remaining buffered data as a final frame.
    ///
    /// Call this when the upstream stream ends. If there is partial data in the
    /// buffer, it is emitted as a frame.
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, ProviderError> {
        // If there is anything left in the buffer, treat it as a line.
        if !self.buffer.is_empty() {
            // Attempt UTF-8 decode of the remaining bytes.
            let text = str::from_utf8(&self.buffer).map_err(ProviderError::from)?;
            let line = text.trim_end_matches('\r').to_owned();
            self.buffer.clear();
            self.process_line(&line);
        }
        let frame = self.take_current_frame();
        Ok(frame.into_iter().collect())
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    /// Parse as many complete lines as possible from the buffer.
    ///
    /// Handles the case where a multi-byte UTF-8 character is split across
    /// two TCP chunks: we only decode up to the last newline, keeping any
    /// trailing bytes (which may be an incomplete UTF-8 sequence) in the
    /// buffer for the next chunk.
    fn drain_buffer(&mut self) -> Result<Vec<SseFrame>, ProviderError> {
        let mut frames = Vec::new();

        while let Some(nl_pos) = self.buffer.iter().position(|&b| b == b'\n') {
            // Extract the line bytes (excluding the newline itself).
            let line_bytes = self.buffer[..nl_pos].to_vec();
            // Remove the line + newline from the buffer.
            self.buffer.drain(..nl_pos + 1);

            // Trim trailing \r bytes.
            let line_bytes = match line_bytes.iter().rposition(|&b| b != b'\r') {
                Some(pos) => &line_bytes[..=pos],
                None => &[][..], // Line was all \r characters.
            };

            // Decode the trimmed line as UTF-8. The line is guaranteed to be
            // valid UTF-8 because it ends at a \n boundary which is a single
            // byte; any partial multi-byte UTF-8 sequence would not contain \n.
            let line = str::from_utf8(line_bytes).map_err(ProviderError::from)?;

            if line.is_empty() {
                // Blank line = frame boundary.
                if let Some(frame) = self.take_current_frame() {
                    frames.push(frame);
                }
            } else {
                self.process_line(line);
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

        // Invalid UTF-8 bytes buffered (no newline, so drain_buffer finds nothing).
        let result = framer.push_chunk(&[0xFF, 0xFE]);
        assert!(
            result.is_ok(),
            "push_chunk should buffer raw bytes without error"
        );
        // The error surfaces when finish() tries to decode the buffer as UTF-8.
        let result = framer.finish();
        assert!(
            result.is_err(),
            "Expected error for invalid UTF-8 on finish"
        );
        match result.unwrap_err() {
            ProviderError::Utf8(_) => {}
            other => panic!("Expected ProviderError::Utf8, got: {:?}", other),
        }
    }

    #[test]
    fn sse_framer_rejects_invalid_utf8_at_newline() {
        let mut framer = SseFramer::new();

        // Invalid UTF-8 followed by a newline: drain_buffer will try to decode
        // the line and fail.
        let result = framer.push_chunk(&[0xFF, 0xFE, b'\n']);
        assert!(result.is_err(), "Expected error for invalid UTF-8 at newline");
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

    #[test]
    fn sse_framer_multibyte_utf8_split_across_chunks() {
        // The character 'あ' is 3 bytes in UTF-8: 0xE3 0x81 0x82.
        let full_char = "あ";
        let full_bytes = full_char.as_bytes();
        assert_eq!(full_bytes.len(), 3);

        let mut framer = SseFramer::new();

        // First chunk: "data: " + first 2 bytes of あ.
        let mut chunk1 = b"data: ".to_vec();
        chunk1.extend_from_slice(&full_bytes[..2]);
        let frames = framer.push_chunk(&chunk1).unwrap();
        assert!(frames.is_empty(), "No complete frame yet");

        // Second chunk: remaining 1 byte of あ + "\n\n".
        let mut chunk2 = full_bytes[2..].to_vec();
        chunk2.extend_from_slice(b"\n\n");
        let frames = framer.push_chunk(&chunk2).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "あ", "Multi-byte char must be preserved across chunk split");
    }

    #[test]
    fn sse_framer_data_newline_then_finish() {
        // "data: hello\n" (newline-terminated line but no blank line after it)
        // push_chunk drains the line but doesn't emit a frame; finish() should
        // emit it.
        let mut framer = SseFramer::new();
        let frames = framer.push_chunk(b"data: hello\n").unwrap();
        assert!(frames.is_empty(), "No blank line yet, no frame");

        let frames = framer.finish().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn sse_framer_reusable_after_finish() {
        // Verify the framer is in a clean state after finish() so it can be
        // reused for a second stream.
        let mut framer = SseFramer::new();

        // First round.
        let frames = framer.push_chunk(b"data: first\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "first");

        let frames = framer.finish().unwrap();
        assert!(frames.is_empty(), "Nothing left after complete stream");

        // Second round -- framer should be clean.
        let frames = framer.push_chunk(b"data: second\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "second");
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
