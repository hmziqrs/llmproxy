//! Stream transformation between API formats.
//!
//! Converts streaming SSE responses from OpenAI Chat Completions, OpenAI
//! Responses API, and Google Gemini into Anthropic SSE events suitable for
//! streaming back to a Claude Code client.
//!
//! The core type is [`StreamProxy`], a stateful struct that tracks content
//! block lifecycle and emits the correct sequence of Anthropic SSE events:
//!
//! ```text
//! message_start
//!   content_block_start  (text | thinking | tool_use)
//!   content_block_delta  (text_delta | thinking_delta | input_json_delta)
//!   content_block_stop
//!   ...
//! message_delta  (stop_reason + usage)
//! message_stop
//! ```
//!
//! The logic is ported from the Go reference implementation in
//! `ref/oc-go-cc/internal/transformer/stream.go`.

use std::collections::HashMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::anthropic::{ContentBlock, Delta, MessageEvent, MessageResponse, Usage};
use crate::openai::{ChatCompletionChunk, UsageInfo};
use crate::zen::{GeminiStreamChunk, ResponsesChunk};
use super::{non_negative, map_finish_reason};

// ---------------------------------------------------------------------------
// Sentinel error
// ---------------------------------------------------------------------------

/// Returned when the client disconnects mid-stream.
pub struct ErrClientDisconnected;

impl fmt::Debug for ErrClientDisconnected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "client disconnected")
    }
}

impl fmt::Display for ErrClientDisconnected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "client disconnected")
    }
}

impl std::error::Error for ErrClientDisconnected {}

/// Generate a unique-enough ID based on the current nanosecond timestamp.
fn generate_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

/// Build Anthropic [`Usage`] from an optional OpenAI [`UsageInfo`], subtracting
/// cache token counts from `input_tokens`.
fn usage_to_anthropic(usage: Option<&UsageInfo>) -> Usage {
    match usage {
        None => Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
        Some(info) => {
            let prompt = info.prompt_tokens as i64;
            let cache_hit = info.prompt_cache_hit_tokens.unwrap_or(0) as i64;
            let cache_miss = info.prompt_cache_miss_tokens.unwrap_or(0) as i64;
            Usage {
                input_tokens: non_negative(prompt - cache_hit - cache_miss) as i32,
                output_tokens: info.completion_tokens,
                cache_creation_input_tokens: info.prompt_cache_miss_tokens,
                cache_read_input_tokens: info.prompt_cache_hit_tokens,
            }
        }
    }
}

/// Write a single SSE event to the writer.
///
/// Format: `event: <type>\ndata: <json>\n\n`
fn write_sse_event(writer: &mut impl fmt::Write, event: &MessageEvent) -> Result<(), String> {
    let data =
        serde_json::to_string(event).map_err(|e| format!("failed to marshal SSE event: {e}"))?;
    write!(writer, "event: {}\ndata: {}\n\n", event.r#type, data)
        .map_err(|e| format!("failed to write SSE event: {e}"))
}

// ---------------------------------------------------------------------------
// StreamProxy
// ---------------------------------------------------------------------------

/// Stateful stream transformer that converts upstream SSE chunks into
/// Anthropic-format SSE events.
///
/// Call [`StreamProxy::process_openai_chunk`], [`StreamProxy::process_responses_chunk`],
/// or [`StreamProxy::process_gemini_chunk`] for each incoming SSE line, then
/// [`StreamProxy::finish`] to emit the closing events.
#[derive(Debug)]
pub struct StreamProxy {
    /// Whether `message_start` has been emitted.
    started: bool,
    /// Whether a text content block is currently open.
    content_started: bool,
    /// Whether a thinking (reasoning) content block is currently open.
    reasoning_started: bool,
    /// Whether `message_delta` (with stop_reason) has been emitted.
    stop_sent: bool,
    /// Whether `finish()` has been called. Prevents double-emission of
    /// `message_stop` when `finish()` is called more than once.
    finished: bool,
    /// Current content block index.
    content_index: usize,
    /// Maps OpenAI tool-call array index to the Anthropic content block index.
    started_tool_calls: HashMap<usize, usize>,
    /// The message ID for this stream.
    msg_id: String,
    /// The model ID to report in events.
    model_id: String,
}

impl StreamProxy {
    /// Create a new stream proxy for the given model.
    pub fn new(model_id: &str) -> Self {
        Self {
            started: false,
            content_started: false,
            reasoning_started: false,
            stop_sent: false,
            finished: false,
            content_index: 0,
            started_tool_calls: HashMap::new(),
            msg_id: format!("msg_{}", generate_id()),
            model_id: model_id.to_owned(),
        }
    }

    // -- message_start --------------------------------------------------------

    /// Emit the `message_start` event if it has not been emitted yet.
    fn ensure_started(&mut self, writer: &mut impl fmt::Write) -> Result<(), String> {
        if self.started {
            return Ok(());
        }
        self.started = true;

        let event = MessageEvent {
            r#type: "message_start".to_owned(),
            message: Some(MessageResponse {
                id: self.msg_id.clone(),
                r#type: "message".to_owned(),
                role: "assistant".to_owned(),
                content: vec![],
                model: self.model_id.clone(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                },
            }),
            index: None,
            content_block: None,
            delta: None,
            usage: None,
            error: None,
        };
        write_sse_event(writer, &event)
    }

    // -- content block helpers ------------------------------------------------

    /// Close the currently open text or reasoning content block (if any).
    fn close_current_block(&mut self, writer: &mut impl fmt::Write) -> Result<(), String> {
        if self.content_started || self.reasoning_started {
            let event = MessageEvent {
                r#type: "content_block_stop".to_owned(),
                message: None,
                index: Some(self.content_index),
                content_block: None,
                delta: None,
                usage: None,
                error: None,
            };
            write_sse_event(writer, &event)?;
            self.content_started = false;
            self.reasoning_started = false;
        }
        Ok(())
    }

    /// Open a new text content block at the current index.
    fn start_text_block(&mut self, writer: &mut impl fmt::Write) -> Result<(), String> {
        self.content_started = true;
        let event = MessageEvent {
            r#type: "content_block_start".to_owned(),
            message: None,
            index: Some(self.content_index),
            content_block: Some(ContentBlock {
                r#type: "text".to_owned(),
                text: Some(String::new()),
                id: None,
                tool_use_id: None,
                name: None,
                input: None,
                output: None,
                content: None,
                is_error: None,
                thinking: None,
                signature: None,
                source: None,
            }),
            delta: None,
            usage: None,
            error: None,
        };
        write_sse_event(writer, &event)
    }

    /// Open a new thinking content block at the current index.
    fn start_thinking_block(&mut self, writer: &mut impl fmt::Write) -> Result<(), String> {
        self.reasoning_started = true;
        let event = MessageEvent {
            r#type: "content_block_start".to_owned(),
            message: None,
            index: Some(self.content_index),
            content_block: Some(ContentBlock {
                r#type: "thinking".to_owned(),
                thinking: Some(String::new()),
                text: None,
                id: None,
                tool_use_id: None,
                name: None,
                input: None,
                output: None,
                content: None,
                is_error: None,
                signature: None,
                source: None,
            }),
            delta: None,
            usage: None,
            error: None,
        };
        write_sse_event(writer, &event)
    }

    /// Emit a `text_delta` event at the current index.
    fn emit_text_delta(&self, writer: &mut impl fmt::Write, text: &str) -> Result<(), String> {
        let event = MessageEvent {
            r#type: "content_block_delta".to_owned(),
            message: None,
            index: Some(self.content_index),
            content_block: None,
            delta: Some(Delta {
                r#type: Some("text_delta".to_owned()),
                text: Some(text.to_owned()),
                thinking: None,
                partial_json: None,
                stop_reason: None,
            }),
            usage: None,
            error: None,
        };
        write_sse_event(writer, &event)
    }

    /// Emit a `thinking_delta` event at the current index.
    fn emit_thinking_delta(
        &self,
        writer: &mut impl fmt::Write,
        thinking: &str,
    ) -> Result<(), String> {
        let event = MessageEvent {
            r#type: "content_block_delta".to_owned(),
            message: None,
            index: Some(self.content_index),
            content_block: None,
            delta: Some(Delta {
                r#type: Some("thinking_delta".to_owned()),
                text: None,
                thinking: Some(thinking.to_owned()),
                partial_json: None,
                stop_reason: None,
            }),
            usage: None,
            error: None,
        };
        write_sse_event(writer, &event)
    }

    // -- finish ---------------------------------------------------------------

    /// Emit closing events for any open blocks, `message_delta`, and
    /// `message_stop`. Must be called once after all chunks have been
    /// processed. Subsequent calls are no-ops to prevent double-emission
    /// of `message_stop`.
    pub fn finish(&mut self, writer: &mut impl fmt::Write) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        self.ensure_started(writer)?;

        // Close any open text or reasoning block.
        self.close_current_block(writer)?;

        // Close any open tool blocks in ascending block-index order.
        if !self.started_tool_calls.is_empty() {
            let mut entries: Vec<_> = self.started_tool_calls.values().copied().collect();
            entries.sort();

            for idx in entries {
                let event = MessageEvent {
                    r#type: "content_block_stop".to_owned(),
                    message: None,
                    index: Some(idx),
                    content_block: None,
                    delta: None,
                    usage: None,
                    error: None,
                };
                write_sse_event(writer, &event)?;
            }
        }

        // Send message_delta if not already sent.
        if !self.stop_sent {
            let stop_reason = if !self.started_tool_calls.is_empty() {
                "tool_use"
            } else {
                "end_turn"
            };
            let event = MessageEvent {
                r#type: "message_delta".to_owned(),
                message: None,
                index: None,
                content_block: None,
                delta: Some(Delta {
                    r#type: None,
                    text: None,
                    thinking: None,
                    partial_json: None,
                    stop_reason: Some(stop_reason.to_owned()),
                }),
                usage: Some(usage_to_anthropic(None)),
                error: None,
            };
            write_sse_event(writer, &event)?;
            self.stop_sent = true;
        }

        // Send message_stop.
        let event = MessageEvent {
            r#type: "message_stop".to_owned(),
            message: None,
            index: None,
            content_block: None,
            delta: None,
            usage: None,
            error: None,
        };
        write_sse_event(writer, &event)
    }

    // =======================================================================
    // OpenAI Chat Completions stream
    // =======================================================================

    /// Process a single SSE line from an OpenAI Chat Completions stream.
    ///
    /// The line should be a raw SSE line (e.g. `data: {...}` or empty).
    /// Returns `Ok(())` on success; the caller can check for client
    /// disconnection separately.
    pub fn process_openai_chunk(
        &mut self,
        line: &str,
        writer: &mut impl fmt::Write,
    ) -> Result<(), String> {
        let line = line.trim();

        // Skip empty lines and non-data lines.
        if line.is_empty() {
            return Ok(());
        }
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => return Ok(()),
        };
        if data.is_empty() {
            return Ok(());
        }

        // Handle [DONE] marker.
        if data == "[DONE]" {
            return Ok(());
        }

        self.ensure_started(writer)?;

        // Fast path: check if this is a simple content chunk without full
        // JSON parsing. Skip the fast path when reasoning_content,
        // finish_reason, tool_calls, or usage are present -- falling through
        // to JSON parsing ensures all fields are handled correctly.
        if !data.contains("\"reasoning_content\"")
            && !data.contains("\"finish_reason\"")
            && !data.contains("\"tool_calls\"")
            && !data.contains("\"usage\"")
        {
            if let Some(content) = fast_extract_delta_content(data) {
                if !content.is_empty() {
                    // Close reasoning block if one is open.
                    if self.reasoning_started {
                        let event = MessageEvent {
                            r#type: "content_block_stop".to_owned(),
                            message: None,
                            index: Some(self.content_index),
                            content_block: None,
                            delta: None,
                            usage: None,
                            error: None,
                        };
                        write_sse_event(writer, &event)?;
                        self.content_index += 1;
                        self.reasoning_started = false;
                    }

                    if !self.content_started {
                        self.start_text_block(writer)?;
                    }

                    self.emit_text_delta(writer, &content)?;
                }
                return Ok(());
            }
        }

        // Full JSON parse path.
        let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => return Ok(()), // skip malformed chunks
        };

        // Usage-only chunk (no choices).
        if chunk.choices.is_empty() {
            if let Some(ref usage) = chunk.usage {
                if self.stop_sent {
                    // Stop reason already sent -- emit usage-only delta.
                    let event = MessageEvent {
                        r#type: "message_delta".to_owned(),
                        message: None,
                        index: None,
                        content_block: None,
                        delta: Some(Delta {
                            r#type: None,
                            text: None,
                            thinking: None,
                            partial_json: None,
                            stop_reason: None,
                        }),
                        usage: Some(usage_to_anthropic(Some(usage))),
                        error: None,
                    };
                    write_sse_event(writer, &event)?;
                } else {
                    let event = MessageEvent {
                        r#type: "message_delta".to_owned(),
                        message: None,
                        index: None,
                        content_block: None,
                        delta: Some(Delta {
                            r#type: None,
                            text: None,
                            thinking: None,
                            partial_json: None,
                            stop_reason: Some("end_turn".to_owned()),
                        }),
                        usage: Some(usage_to_anthropic(Some(usage))),
                        error: None,
                    };
                    write_sse_event(writer, &event)?;
                    self.stop_sent = true;
                }
            }
            return Ok(());
        }

        let choice = &chunk.choices[0];

        // Handle reasoning content deltas.
        if let Some(reasoning) = choice
            .delta
            .as_ref()
            .and_then(|d| d.reasoning_content.as_ref())
        {
            if !reasoning.is_empty() {
                // Close text block if open.
                if self.content_started {
                    let event = MessageEvent {
                        r#type: "content_block_stop".to_owned(),
                        message: None,
                        index: Some(self.content_index),
                        content_block: None,
                        delta: None,
                        usage: None,
                        error: None,
                    };
                    write_sse_event(writer, &event)?;
                    self.content_index += 1;
                    self.content_started = false;
                }

                if !self.reasoning_started {
                    self.start_thinking_block(writer)?;
                }

                self.emit_thinking_delta(writer, reasoning)?;
            }
        }

        // Handle text content deltas.
        if let Some(ref delta) = choice.delta {
            if !delta.content.is_empty() {
                // Close reasoning block if open.
                if self.reasoning_started {
                    let event = MessageEvent {
                        r#type: "content_block_stop".to_owned(),
                        message: None,
                        index: Some(self.content_index),
                        content_block: None,
                        delta: None,
                        usage: None,
                        error: None,
                    };
                    write_sse_event(writer, &event)?;
                    self.content_index += 1;
                    self.reasoning_started = false;
                }

                if !self.content_started {
                    self.start_text_block(writer)?;
                }

                self.emit_text_delta(writer, &delta.content)?;
            }
        }

        // Handle tool call deltas.
        // We separate the "is new?" check from the mutation to avoid
        // borrowing self.started_tool_calls mutably across method calls.
        if let Some(ref delta) = choice.delta {
            if !delta.tool_calls.is_empty() {
                // Phase 1: determine which tool calls need new blocks.
                struct NewToolInfo {
                    openai_index: usize,
                    tool_id: String,
                    name: String,
                }
                let mut new_tools: Vec<NewToolInfo> = Vec::new();

                for tc in &delta.tool_calls {
                    let oi = tc.index.unwrap_or(0) as usize;

                    if self.started_tool_calls.contains_key(&oi) {
                        continue; // Already started — will handle delta below.
                    }

                    let func_name = tc
                        .function
                        .as_ref()
                        .and_then(|f| f.name.as_deref())
                        .unwrap_or("");
                    if func_name.is_empty() {
                        // Ghost chunk: index recycled, ignore.
                        continue;
                    }

                    let tool_id = tc
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("toolu_{}", generate_id()));

                    new_tools.push(NewToolInfo {
                        openai_index: oi,
                        tool_id,
                        name: func_name.to_owned(),
                    });
                }

                // Phase 2: close any open text/reasoning block (at most once).
                if !new_tools.is_empty() {
                    self.close_current_block(writer)?;
                }

                // Phase 3: open new tool blocks and register them.
                for nt in &new_tools {
                    self.content_index += 1;
                    let block_idx = self.content_index;
                    self.started_tool_calls.insert(nt.openai_index, block_idx);

                    let start_event = MessageEvent {
                        r#type: "content_block_start".to_owned(),
                        message: None,
                        index: Some(block_idx),
                        content_block: Some(ContentBlock {
                            r#type: "tool_use".to_owned(),
                            id: Some(nt.tool_id.clone()),
                            name: Some(nt.name.clone()),
                            input: Some(serde_json::Value::Object(serde_json::Map::new())),
                            text: None,
                            tool_use_id: None,
                            output: None,
                            content: None,
                            is_error: None,
                            thinking: None,
                            signature: None,
                            source: None,
                        }),
                        delta: None,
                        usage: None,
                        error: None,
                    };
                    write_sse_event(writer, &start_event)?;
                }

                // Phase 4: send argument deltas for all tool calls (new + existing).
                for tc in &delta.tool_calls {
                    let oi = tc.index.unwrap_or(0) as usize;
                    if let Some(ref func) = tc.function {
                        if let Some(ref args) = func.arguments {
                            if !args.is_empty() {
                                if let Some(&block_idx) = self.started_tool_calls.get(&oi) {
                                    let event = MessageEvent {
                                        r#type: "content_block_delta".to_owned(),
                                        message: None,
                                        index: Some(block_idx),
                                        content_block: None,
                                        delta: Some(Delta {
                                            r#type: Some("input_json_delta".to_owned()),
                                            text: None,
                                            thinking: None,
                                            partial_json: Some(args.clone()),
                                            stop_reason: None,
                                        }),
                                        usage: None,
                                        error: None,
                                    };
                                    write_sse_event(writer, &event)?;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Handle finish reason.
        if let Some(ref reason) = choice.finish_reason {
            if !reason.is_empty() {
                // Close any open text/reasoning block.
                self.close_current_block(writer)?;

                // Close any open tool blocks in ascending index order.
                if !self.started_tool_calls.is_empty() {
                    let mut entries: Vec<_> = self.started_tool_calls.values().copied().collect();
                    entries.sort();

                    for idx in entries {
                        let event = MessageEvent {
                            r#type: "content_block_stop".to_owned(),
                            message: None,
                            index: Some(idx),
                            content_block: None,
                            delta: None,
                            usage: None,
                            error: None,
                        };
                        write_sse_event(writer, &event)?;
                    }
                    self.started_tool_calls.clear();
                }

                let stop_reason = map_finish_reason(reason);
                let event = MessageEvent {
                    r#type: "message_delta".to_owned(),
                    message: None,
                    index: None,
                    content_block: None,
                    delta: Some(Delta {
                        r#type: None,
                        text: None,
                        thinking: None,
                        partial_json: None,
                        stop_reason: Some(stop_reason.to_owned()),
                    }),
                    usage: Some(usage_to_anthropic(chunk.usage.as_ref())),
                    error: None,
                };
                write_sse_event(writer, &event)?;
                self.stop_sent = true;
            }
        }

        Ok(())
    }

    // =======================================================================
    // Responses API stream
    // =======================================================================

    /// Process a single SSE line from an OpenAI Responses API stream.
    pub fn process_responses_chunk(
        &mut self,
        line: &str,
        writer: &mut impl fmt::Write,
    ) -> Result<(), String> {
        let line = line.trim();

        if line.is_empty() {
            return Ok(());
        }
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => return Ok(()),
        };
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }

        self.ensure_started(writer)?;

        let chunk: ResponsesChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

        // Text delta.
        if chunk.r#type == "response.output_text.delta" {
            if let Some(ref delta_text) = chunk.delta {
                if !delta_text.is_empty() {
                    if !self.content_started {
                        self.start_text_block(writer)?;
                    }

                    self.emit_text_delta(writer, delta_text)?;
                }
            }
        }

        // Stream completion events.
        if (chunk.r#type == "response.completed" || chunk.r#type == "response.done")
            && !self.stop_sent
        {
            // Close text block if open.
            self.close_current_block(writer)?;

            let event = MessageEvent {
                r#type: "message_delta".to_owned(),
                message: None,
                index: None,
                content_block: None,
                delta: Some(Delta {
                    r#type: None,
                    text: None,
                    thinking: None,
                    partial_json: None,
                    stop_reason: Some("end_turn".to_owned()),
                }),
                usage: Some(usage_to_anthropic(None)),
                error: None,
            };
            write_sse_event(writer, &event)?;
            self.stop_sent = true;
        }

        Ok(())
    }

    // =======================================================================
    // Gemini stream
    // =======================================================================

    /// Process a single SSE line from a Google Gemini stream.
    pub fn process_gemini_chunk(
        &mut self,
        line: &str,
        writer: &mut impl fmt::Write,
    ) -> Result<(), String> {
        let line = line.trim();

        if line.is_empty() {
            return Ok(());
        }
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => return Ok(()),
        };
        if data.is_empty() {
            return Ok(());
        }

        self.ensure_started(writer)?;

        let chunk: GeminiStreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

        if chunk.candidates.is_empty() {
            return Ok(());
        }

        let candidate = &chunk.candidates[0];

        // Emit text parts.
        for part in &candidate.content.parts {
            if let Some(ref text) = part.text {
                if !text.is_empty() {
                    if !self.content_started {
                        self.start_text_block(writer)?;
                    }

                    self.emit_text_delta(writer, text)?;
                }
            }
        }

        // Handle finish reason.
        if let Some(ref reason) = candidate.finish_reason {
            if !reason.is_empty() && !self.stop_sent {
                // Close text block if open.
                self.close_current_block(writer)?;

                let stop_reason = match reason.as_str() {
                    "MAX_TOKENS" => "max_tokens",
                    _ => "end_turn",
                };

                let event = MessageEvent {
                    r#type: "message_delta".to_owned(),
                    message: None,
                    index: None,
                    content_block: None,
                    delta: Some(Delta {
                        r#type: None,
                        text: None,
                        thinking: None,
                        partial_json: None,
                        stop_reason: Some(stop_reason.to_owned()),
                    }),
                    usage: Some(usage_to_anthropic(None)),
                    error: None,
                };
                write_sse_event(writer, &event)?;
                self.stop_sent = true;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fast-path content extraction
// ---------------------------------------------------------------------------

/// Attempt to extract the content string from a `"delta":{"content":"..."`}
/// pattern using plain string search, avoiding a full JSON parse.
///
/// Returns `None` if the pattern is not found, the content contains escape
/// sequences (e.g. `\"`), or the extraction otherwise cannot be done safely.
/// In those cases the caller falls through to a full JSON parse.
fn fast_extract_delta_content(data: &str) -> Option<String> {
    let marker = r#""delta":{"content":""#;
    let start = data.find(marker)?;
    let content_start = start + marker.len();

    // Scan for the closing unescaped double quote.
    let remaining = &data[content_start..];
    let mut i = 0;
    let bytes = remaining.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            // Escape sequence present -- fall through to full parse.
            return None;
        }
        if bytes[i] == b'"' {
            let content = &remaining[..i];
            return Some(content.to_owned());
        }
        i += 1;
    }
    None
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- fast_extract_delta_content -------------------------------------------

    #[test]
    fn fast_path_extracts_simple_content() {
        let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        assert_eq!(fast_extract_delta_content(data), Some("hello".to_owned()));
    }

    #[test]
    fn fast_path_returns_none_when_no_delta() {
        let data = r#"{"id":"chatcmpl-1","choices":[{"delta":{"role":"assistant"}}]}"#;
        assert_eq!(fast_extract_delta_content(data), None);
    }

    // -- usage_to_anthropic ---------------------------------------------------

    #[test]
    fn usage_none_returns_zeros() {
        let u = usage_to_anthropic(None);
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert_eq!(u.cache_creation_input_tokens, None);
        assert_eq!(u.cache_read_input_tokens, None);
    }

    #[test]
    fn usage_with_cache_tokens() {
        let info = UsageInfo {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_cache_hit_tokens: Some(30),
            prompt_cache_miss_tokens: Some(20),
        };
        let u = usage_to_anthropic(Some(&info));
        // input = 100 - 30 - 20 = 50
        assert_eq!(u.input_tokens, 50);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_creation_input_tokens, Some(20));
        assert_eq!(u.cache_read_input_tokens, Some(30));
    }

    // -- map_finish_reason ----------------------------------------------------

    #[test]
    fn finish_reason_mappings() {
        assert_eq!(map_finish_reason("stop"), "end_turn");
        assert_eq!(map_finish_reason("length"), "max_tokens");
        assert_eq!(map_finish_reason("tool_calls"), "tool_use");
        assert_eq!(map_finish_reason("tool_use"), "tool_use");
        assert_eq!(map_finish_reason("content_filter"), "end_turn");
        assert_eq!(map_finish_reason("unknown"), "end_turn");
    }

    // -- StreamProxy: OpenAI text stream --------------------------------------

    #[test]
    fn openai_simple_text_stream() {
        let mut proxy = StreamProxy::new("test-model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];

        for chunk in &chunks {
            proxy.process_openai_chunk(chunk, &mut out).unwrap();
        }
        // finish emits message_stop.
        proxy.finish(&mut out).unwrap();

        assert!(out.contains("event: message_start"));
        assert!(out.contains("event: content_block_start"));
        assert!(out.contains("event: content_block_delta"));
        assert!(out.contains("Hello"));
        assert!(out.contains(" world"));
        assert!(out.contains("event: content_block_stop"));
        assert!(out.contains("event: message_delta"));
        assert!(out.contains("end_turn"));
        assert!(out.contains("event: message_stop"));
    }

    // -- StreamProxy: OpenAI reasoning + text ---------------------------------

    #[test]
    fn openai_reasoning_then_text() {
        let mut proxy = StreamProxy::new("think-model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"reasoning_content":"thinking...","content":""},"finish_reason":null}]}"#,
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"reasoning_content":"","content":"answer"},"finish_reason":null}]}"#,
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ];

        for chunk in &chunks {
            proxy.process_openai_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("thinking_delta"));
        assert!(out.contains("thinking..."));
        assert!(out.contains("text_delta"));
        assert!(out.contains("answer"));
    }

    // -- StreamProxy: Responses API stream ------------------------------------

    #[test]
    fn responses_text_stream() {
        let mut proxy = StreamProxy::new("resp-model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"type":"response.output_text.delta","delta":"Hello"}"#,
            r#"data: {"type":"response.output_text.delta","delta":" world"}"#,
            r#"data: {"type":"response.completed"}"#,
        ];

        for chunk in &chunks {
            proxy.process_responses_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("event: message_start"));
        assert!(out.contains("Hello"));
        assert!(out.contains(" world"));
        assert!(out.contains("event: message_delta"));
        assert!(out.contains("end_turn"));
    }

    // -- StreamProxy: Gemini stream -------------------------------------------

    #[test]
    fn gemini_text_stream() {
        let mut proxy = StreamProxy::new("gemini-model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Hi"}]},"finishReason":null}]}"#,
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":" there"}]},"finishReason":"STOP"}]}"#,
        ];

        for chunk in &chunks {
            proxy.process_gemini_chunk(chunk, &mut out).unwrap();
        }
        proxy.finish(&mut out).unwrap();

        assert!(out.contains("event: message_start"));
        assert!(out.contains("Hi"));
        assert!(out.contains(" there"));
        assert!(out.contains("event: content_block_stop"));
        assert!(out.contains("event: message_delta"));
        assert!(out.contains("end_turn"));
    }

    #[test]
    fn gemini_max_tokens_stop() {
        let mut proxy = StreamProxy::new("gemini-model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"cut off"}]},"finishReason":"MAX_TOKENS"}]}"#,
        ];

        for chunk in &chunks {
            proxy.process_gemini_chunk(chunk, &mut out).unwrap();
        }

        // The finish_reason handler emits message_delta with "max_tokens"
        // and closes the content block.
        assert!(out.contains("max_tokens"));
    }

    // -- StreamProxy: finish() closes open blocks -----------------------------

    #[test]
    fn finish_closes_open_text_block() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        // Only send content, never a finish_reason.
        let chunk = r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        proxy.process_openai_chunk(chunk, &mut out).unwrap();

        proxy.finish(&mut out).unwrap();

        assert!(out.contains("event: content_block_stop"));
        assert!(out.contains("event: message_delta"));
        assert!(out.contains("event: message_stop"));
    }

    // -- StreamProxy: skip non-data lines -------------------------------------

    #[test]
    fn skip_non_data_lines() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        proxy.process_openai_chunk("", &mut out).unwrap();
        proxy.process_openai_chunk("event: ping", &mut out).unwrap();
        proxy.process_openai_chunk(": keepalive", &mut out).unwrap();
        proxy.finish(&mut out).unwrap();

        // Should only have start + finish events, no errors.
        assert!(out.contains("event: message_start"));
        assert!(out.contains("event: message_stop"));
    }

    // -- StreamProxy: tool calls stream ---------------------------------------

    #[test]
    fn openai_tool_call_stream() {
        let mut proxy = StreamProxy::new("tool-model");
        let mut out = String::new();

        let chunks = [
            // First chunk: text content
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"calling tool"},"finish_reason":null}]}"#,
            // Tool call start: id + name
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
            // Tool call argument delta
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]},"finish_reason":null}]}"#,
            // Tool call argument continuation
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"SF\"}"}}]},"finish_reason":null}]}"#,
            // Finish
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];

        for chunk in &chunks {
            proxy.process_openai_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("calling tool"));
        assert!(out.contains("tool_use"));
        assert!(out.contains("get_weather"));
        assert!(out.contains("input_json_delta"));
        // The partial_json args are JSON-escaped inside the SSE event data.
        // {"city": -> escaped as {\"city\":
        assert!(out.contains("city"));
        assert!(out.contains("SF"));
        assert!(out.contains("event: message_delta"));
    }

    // -- SSE format -----------------------------------------------------------

    #[test]
    fn sse_event_format() {
        let event = MessageEvent {
            r#type: "message_stop".to_owned(),
            message: None,
            index: None,
            content_block: None,
            delta: None,
            usage: None,
            error: None,
        };
        let mut out = String::new();
        write_sse_event(&mut out, &event).unwrap();

        assert!(out.starts_with("event: message_stop\ndata: "));
        assert!(out.ends_with("\n\n"));
        assert!(out.contains("\"type\":\"message_stop\""));
    }

    // =========================================================================
    // Phase 0 characterization tests
    // =========================================================================
    //
    // These tests verify current stream behavior for edge cases so that the
    // migration to the core-protocol architecture does not silently change
    // how malformed events, unknown fields, and disconnect-like conditions
    // are handled. They are legacy characterization tests.
    // New tests after Phase 0 must target `wire -> core -> wire`, not direct
    // protocol pairs.

    // -- ErrClientDisconnected ------------------------------------------------

    #[test]
    fn err_client_disconnected_display() {
        assert_eq!(ErrClientDisconnected.to_string(), "client disconnected");
    }

    #[test]
    fn err_client_disconnected_debug() {
        assert_eq!(format!("{:?}", ErrClientDisconnected), "client disconnected");
    }

    // -- Malformed SSE events: OpenAI stream ----------------------------------

    /// Characterization: malformed JSON inside a `data:` line is silently
    /// skipped by `process_openai_chunk`. No error is propagated.
    #[test]
    fn openai_malformed_json_chunk_is_skipped() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"before"},"finish_reason":null}]}"#,
            r#"data: {not valid json}"#,
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"after"},"finish_reason":null}]}"#,
        ];

        for chunk in &chunks {
            // Must not return an error for malformed JSON.
            proxy.process_openai_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("before"));
        assert!(out.contains("after"));
        // The malformed chunk should not produce any content output.
        assert!(!out.contains("not valid json"));
    }

    /// Characterization: a chunk with no `choices` array but valid usage
    /// emits a usage-only delta. This is the standard OpenAI usage-chunk
    /// pattern when `stream_options.include_usage` is set.
    #[test]
    fn openai_usage_only_chunk_emits_usage() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#,
            r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":10,"total_tokens":60}}"#,
        ];

        for chunk in &chunks {
            proxy.process_openai_chunk(chunk, &mut out).unwrap();
        }

        // The stop-reason chunk emits message_delta with end_turn.
        // The usage-only chunk (no choices) should also emit a message_delta
        // with usage information since stop was already sent.
        assert!(out.contains("event: message_delta"));
    }

    /// Characterization: unknown fields in OpenAI chunk JSON are now rejected
    /// by `deny_unknown_fields` on `ChatCompletionChunk`. The chunk is silently
    /// skipped (no error propagated, no content emitted).
    #[test]
    fn openai_unknown_fields_rejected_chunk_skipped() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        // Chunk with unknown fields is rejected by deny_unknown_fields.
        let chunk_with_unknown = r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}],"custom_field":"ignored"}"#;

        proxy.process_openai_chunk(chunk_with_unknown, &mut out).unwrap();

        // The chunk should be silently skipped -- no content, no error.
        assert!(!out.contains("hello"));
        assert!(!out.contains("custom_field"));
    }

    /// Characterization: a chunk without unknown fields is processed normally.
    #[test]
    fn openai_chunk_without_unknown_fields_works() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunk = r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;

        proxy.process_openai_chunk(chunk, &mut out).unwrap();

        assert!(out.contains("hello"));
    }

    // -- Malformed SSE events: Responses API stream ---------------------------

    /// Characterization: malformed JSON in a Responses API chunk is silently
    /// skipped.
    #[test]
    fn responses_malformed_json_chunk_is_skipped() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"type":"response.output_text.delta","delta":"before"}"#,
            r#"data: {not valid json}"#,
            r#"data: {"type":"response.output_text.delta","delta":"after"}"#,
        ];

        for chunk in &chunks {
            proxy.process_responses_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(!out.contains("not valid json"));
    }

    /// Characterization: unknown Responses API event types are silently
    /// ignored (no panic, no error).
    #[test]
    fn responses_unknown_event_type_is_ignored() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"type":"response.output_text.delta","delta":"hello"}"#,
            r#"data: {"type":"response.custom_event","delta":"ignored"}"#,
        ];

        for chunk in &chunks {
            proxy.process_responses_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("hello"));
        assert!(!out.contains("custom_event"));
    }

    // -- Malformed SSE events: Gemini stream ----------------------------------

    /// Characterization: malformed JSON in a Gemini chunk is silently skipped.
    #[test]
    fn gemini_malformed_json_chunk_is_skipped() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"before"}]}}]}"#,
            r#"data: {not valid json}"#,
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"after"}]}}]}"#,
        ];

        for chunk in &chunks {
            proxy.process_gemini_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(!out.contains("not valid json"));
    }

    /// Characterization: Gemini chunk with empty candidates array is silently
    /// skipped (no content emitted, no error).
    #[test]
    fn gemini_empty_candidates_is_skipped() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        let chunk = r#"data: {"candidates":[]}"#;
        proxy.process_gemini_chunk(chunk, &mut out).unwrap();

        // Should only have message_start, no content.
        assert!(out.contains("event: message_start"));
        assert!(!out.contains("content_block_start"));
    }

    // -- Disconnect-like behavior ---------------------------------------------

    /// Characterization: when the stream ends abruptly (no finish_reason,
    /// no [DONE]), `finish()` still emits proper closing events.
    #[test]
    fn abrupt_stream_end_finish_emits_closing_events() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        // Send a text chunk but never send finish_reason.
        let chunk = r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        proxy.process_openai_chunk(chunk, &mut out).unwrap();

        // Simulate abrupt disconnect -- just call finish().
        proxy.finish(&mut out).unwrap();

        // Verify we get a clean closing sequence.
        assert!(out.contains("event: content_block_start"));
        assert!(out.contains("hello"));
        assert!(out.contains("event: content_block_stop"));
        assert!(out.contains("event: message_delta"));
        assert!(out.contains("end_turn"));
        assert!(out.contains("event: message_stop"));
    }

    /// Characterization: `finish()` when no content has been sent at all
    /// still emits a valid message_start + message_delta + message_stop
    /// sequence.
    #[test]
    fn finish_with_no_prior_content_emits_lifecycle() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        proxy.finish(&mut out).unwrap();

        assert!(out.contains("event: message_start"));
        assert!(out.contains("event: message_delta"));
        assert!(out.contains("end_turn"));
        assert!(out.contains("event: message_stop"));
    }

    // -- Provider-specific unsupported fields in request transformation --------

    /// Characterization: unknown fields in Anthropic request JSON are now
    /// rejected by serde (`deny_unknown_fields` on `MessageRequest`). This
    /// prevents silent data loss when clients send fields the proxy does not
    /// model. Previously unknown fields were silently dropped.
    #[test]
    fn request_transform_rejects_unknown_anthropic_fields() {
        use crate::anthropic::MessageRequest;

        let raw = serde_json::json!({
            "model": "test-model",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }],
            "future_field": "not yet supported",
            "experimental": { "enabled": true }
        });

        let result = serde_json::from_value::<MessageRequest>(raw);
        assert!(result.is_err(), "unknown fields should be rejected by deny_unknown_fields");
    }

    /// Characterization: unknown fields in OpenAI response JSON are now
    /// rejected by serde (`deny_unknown_fields` on `ChatCompletionResponse`).
    #[test]
    fn response_transform_rejects_unknown_openai_fields() {
        use crate::openai::ChatCompletionResponse;

        let raw = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1234,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "hi"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            },
            "system_fingerprint": "fp_abc123",
            "service_tier": "default"
        });

        let result = serde_json::from_value::<ChatCompletionResponse>(raw);
        assert!(result.is_err(), "unknown fields should be rejected by deny_unknown_fields");
    }

    // =========================================================================
    // Phase 0: Additional characterization tests
    // =========================================================================

    // -- fast_extract_delta_content: escaped quotes ---------------------------

    /// Characterization: when content contains an escaped quote (`\"`),
    /// `fast_extract_delta_content` returns `None` and the caller falls
    /// through to a full JSON parse.
    #[test]
    fn fast_path_returns_none_for_escaped_quotes() {
        let data = r#"{"id":"c","choices":[{"delta":{"content":"he said \"hello\""},"finish_reason":null}]}"#;
        assert_eq!(fast_extract_delta_content(data), None);
    }

    /// Characterization: backslash-escaped characters in content cause the
    /// fast path to return None.
    #[test]
    fn fast_path_returns_none_for_backslash_escapes() {
        let data = r#"{"id":"c","choices":[{"delta":{"content":"line1\nline2"},"finish_reason":null}]}"#;
        assert_eq!(fast_extract_delta_content(data), None);
    }

    // -- StreamProxy: double finish() is a no-op -------------------------------

    /// Characterization: calling `finish()` twice does not produce duplicate
    /// `message_stop` events. The second call is a no-op.
    #[test]
    fn double_finish_does_not_duplicate_message_stop() {
        let mut proxy = StreamProxy::new("model");
        let mut out = String::new();

        // Send some content.
        let chunk = r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        proxy.process_openai_chunk(chunk, &mut out).unwrap();

        // First finish.
        proxy.finish(&mut out).unwrap();
        let first_stop_count = out.matches("event: message_stop").count();
        assert_eq!(first_stop_count, 1, "should have exactly one message_stop after first finish");

        // Second finish -- should be a no-op.
        proxy.finish(&mut out).unwrap();
        let second_stop_count = out.matches("event: message_stop").count();
        assert_eq!(second_stop_count, 1, "should still have exactly one message_stop after double finish");
    }

    // -- Responses API: tool call streaming ------------------------------------

    /// Characterization: Responses API function_call_arguments.delta events
    /// produce correct Anthropic SSE output with tool_use blocks.
    #[test]
    fn responses_function_call_stream() {
        let mut proxy = StreamProxy::new("resp-model");
        let mut out = String::new();

        // Note: The current Responses API processor only handles text deltas
        // and completion events. Function call deltas are not yet modeled.
        // This test verifies that unknown event types are silently ignored.
        let chunks = [
            r#"data: {"type":"response.output_text.delta","delta":"calling "}"#,
            r#"data: {"type":"response.output_text.delta","delta":"tool"}"#,
            r#"data: {"type":"response.completed"}"#,
        ];

        for chunk in &chunks {
            proxy.process_responses_chunk(chunk, &mut out).unwrap();
        }

        assert!(out.contains("calling "));
        assert!(out.contains("tool"));
        assert!(out.contains("event: message_delta"));
    }

    // -- Gemini: tool call streaming gap ---------------------------------------

    /// Characterization: Gemini streaming with function calls is not yet
    /// implemented in the stream transformer. This test verifies that the
    /// text parts of a Gemini response with function calls are still handled.
    #[test]
    fn gemini_text_stream_with_no_function_calls() {
        let mut proxy = StreamProxy::new("gemini-model");
        let mut out = String::new();

        let chunks = [
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"I'll search for that."}]},"finishReason":null}]}"#,
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":" Done."}]},"finishReason":"STOP"}]}"#,
        ];

        for chunk in &chunks {
            proxy.process_gemini_chunk(chunk, &mut out).unwrap();
        }
        proxy.finish(&mut out).unwrap();

        assert!(out.contains("I'll search for that."));
        assert!(out.contains(" Done."));
        assert!(out.contains("event: message_stop"));
    }
}
