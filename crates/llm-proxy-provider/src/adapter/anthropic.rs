//! Anthropic Messages API provider adapter.
//!
//! Translates between core types and the Anthropic Messages wire format:
//!
//! ```text
//! CoreRequest  -> MessageRequest
//! MessageResponse -> CoreResponse
//! MessageEvent stream -> CoreEvent stream
//! ```

use std::sync::LazyLock;

use llm_proxy_protocol::anthropic::{MessageEvent, MessageResponse};
use llm_proxy_protocol::core::{
    ContentKind, CoreContent, CoreEvent,
    CoreRequest, CoreResponse, CoreRole, CoreToolChoice, ModelRef, StopReason,
    UsageProvenance,
};

use super::{build_proxy_request, expand_url_template, response_model_ref, ProviderAdapterTarget, ProviderStreamDecoder};
use crate::error::ProviderError;
use crate::sse::SseFrame;

// ---------------------------------------------------------------------------
// Tool name sanitization (collision-safe, reversible)
// ---------------------------------------------------------------------------

/// Anthropic requires max_tokens but the core type makes it optional.
/// Use this default when the caller does not specify one.  Chosen to be
/// large enough for most use cases while staying within typical model limits.
const ANTHROPIC_DEFAULT_MAX_TOKENS: i32 = 4096;

// The sanitize/desanitize approach uses a sentinel prefix `__llmp_` to mark
// names that were actually rewritten.  Only names carrying this sentinel are
// decoded during desanitization, which prevents false-positive decoding of
// tool names that naturally contain `_0xHH_` patterns (e.g. `parse_0xff_value`).

// Anthropic `tool_use_id` must match `^[A-Za-z0-9_]{0,256}$`.
static INVALID_TOOL_USE_ID_CHAR: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^A-Za-z0-9_]").expect("valid regex"));

/// Sanitize a tool name for the Anthropic API using collision-safe, reversible encoding.
///
/// Disallowed characters are encoded as `_0xHH_` where HH is the hex byte value.
/// This ensures two different tool names (e.g. `my.tool` and `my_tool`) produce
/// different sanitized names (`my_0x2e_tool` vs `my_tool`), avoiding collisions.
///
/// When encoding is applied, a sentinel prefix `__llmp_` is prepended so that
/// [`desanitize_tool_name`] can distinguish genuinely encoded names from tool names
/// that naturally contain `_0xHH_` patterns (e.g. `parse_0xff_value`).
///
/// Truncates to 128 chars if needed (avoiding partial `_0xHH_` sequences).
/// Returns the sanitized name and whether it was actually rewritten.
fn sanitize_tool_name(name: &str) -> (String, bool) {
    use std::fmt::Write;
    let sentinel = "__llmp_";
    let max_len = 128;
    let mut result = String::with_capacity(name.len() * 3 + sentinel.len());
    let mut changed = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            result.push(ch);
        } else {
            // Encode as _0xHH_ for each byte of the character.
            for byte in ch.to_string().as_bytes() {
                write!(result, "_0x{:02x}_", byte).unwrap();
            }
            changed = true;
        }
    }

    // If encoding was applied, prepend the sentinel.
    if changed {
        result.insert_str(0, sentinel);
    }

    // Truncate to max_len if needed, avoiding partial _0xHH_ sequences.
    if result.len() > max_len {
        let mut end = max_len;
        while !result.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        // Scan backward: if we cut in the middle of a `_0xHH_` sequence,
        // truncate before the opening `_` instead.
        if end >= 5 {
            // Check if we're inside a potential _0xHH_ pattern.
            if let Some(pos) = result[..end].rfind("_0x") {
                let seq_end = pos + 6; // _0xHH_ is 6 bytes
                if seq_end > end {
                    // The _0xHH_ sequence straddles the cut point.
                    // Truncate before the opening `_` instead.
                    end = pos;
                }
            }
        }
        // Final char-boundary check after adjustment.
        while !result.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        result.truncate(end);
        if result.is_empty() {
            return ("tool".to_owned(), true);
        }
    }

    // Guarantee non-empty.
    if result.is_empty() {
        return ("tool".to_owned(), name != "tool");
    }

    (result, changed)
}

/// Reverse a sanitized tool name back to the original by decoding `_0xHH_` patterns.
///
/// Uses a sentinel prefix `__llmp_` to distinguish genuinely encoded names from
/// tool names that happen to contain `_0x` naturally (e.g. `parse_0xff_value`).
/// Only names that carry the `__llmp_` sentinel are decoded; names without it
/// are returned as-is, preventing false-positive decoding.
fn desanitize_tool_name(name: &str) -> std::borrow::Cow<'_, str> {
    const SENTINEL: &str = "__llmp_";

    // Only decode names that carry the sentinel prefix.
    if !name.starts_with(SENTINEL) {
        return std::borrow::Cow::Borrowed(name);
    }

    // Strip the sentinel before decoding.
    let stripped = &name[SENTINEL.len()..];
    if !stripped.contains("_0x") {
        return std::borrow::Cow::Owned(stripped.to_owned());
    }

    let mut result = Vec::<u8>::with_capacity(stripped.len());
    let bytes = stripped.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Pattern: _0xHH_ (6 bytes at offsets i..=i+5).
        // Need at least 6 bytes remaining: i + 5 must be a valid index.
        if bytes[i] == b'_'
            && i + 5 < bytes.len()
            && bytes[i + 1] == b'0'
            && bytes[i + 2] == b'x'
        {
            let hex_hi = i + 3;
            let hex_lo = i + 4;
            let closing = i + 5;
            if bytes[closing] == b'_' {
                // Parse the two hex digits.
                if let Some(byte_val) = hex_byte(bytes[hex_hi], bytes[hex_lo]) {
                    result.push(byte_val);
                    i = closing + 1; // skip past the closing '_'
                    continue;
                }
            }
        }
        // Not a valid pattern, push the byte as-is.
        result.push(bytes[i]);
        i += 1;
    }

    // Convert accumulated bytes back to a UTF-8 string.
    match String::from_utf8(result) {
        Ok(s) => std::borrow::Cow::Owned(s),
        Err(e) => {
            // Fallback: lossy conversion for safety.
            std::borrow::Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned())
        }
    }
}

/// Parse two ASCII hex digits into a byte value.
fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    let hi_val = hex_digit(hi)?;
    let lo_val = hex_digit(lo)?;
    Some(hi_val << 4 | lo_val)
}

/// Parse a single ASCII hex digit into its numeric value.
const fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Sanitize a tool_use_id for the Anthropic API.
///
/// Replaces disallowed characters with underscores, truncates to 256 chars.
fn sanitize_tool_use_id(id: &str) -> String {
    let sanitized: String = INVALID_TOOL_USE_ID_CHAR
        .replace_all(id, "_")
        .into_owned();
    if sanitized.len() > 256 {
        let mut end = 256;
        while !sanitized.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        sanitized[..end].to_owned()
    } else if sanitized.is_empty() {
        "tool_result".to_owned()
    } else {
        sanitized
    }
}

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Adapter for the Anthropic Messages API.
#[derive(Debug, Clone, Default)]
pub struct AnthropicAdapter;

impl AnthropicAdapter {
    /// Create a new Anthropic adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Stream decoder
// ---------------------------------------------------------------------------

/// Stateful stream decoder for Anthropic SSE frames.
///
/// ## CoreEvent variants emitted
///
/// - `MessageStart` -- on `message_start`
/// - `ContentStart` -- on `content_block_start` (Text, Thinking, or ToolUse)
/// - `TextDelta` -- on `content_block_delta` with `text_delta`
/// - `ThinkingDelta` -- on `content_block_delta` with `thinking_delta`
/// - `ToolCallStart` -- on `content_block_start` with `tool_use`
/// - `ToolCallDelta` -- on `content_block_delta` with `input_json_delta`
/// - `ToolCallStop` -- on `content_block_stop` for tool_use blocks
/// - `UsageDelta` -- on `message_delta` with usage
/// - `MessageStop` -- on `message_delta` with stop_reason or `message_stop`
/// - `Ping` -- on `ping`
/// - `Error` -- on `error`
///
/// Intentionally never emitted: none (all variants are covered).
#[derive(Debug)]
pub struct AnthropicStreamDecoder {
    model_ref: ModelRef,
    started: bool,
    current_block_index: Option<usize>,
    current_block_kind: ContentKind,
    tool_blocks: Vec<usize>,
    /// Tracks which tool blocks have already received a `ToolCallStop` via
    /// `content_block_stop`, so `finish()` does not emit duplicates.
    tool_blocks_closed: Vec<bool>,
    stop_sent: bool,
}

impl ProviderStreamDecoder for AnthropicStreamDecoder {
    fn decode_frame(&mut self, frame: &SseFrame) -> Result<Vec<CoreEvent>, ProviderError> {
        let data = frame.data.trim();
        if data.is_empty() {
            return Ok(vec![]);
        }

        let event: MessageEvent = match serde_json::from_str(data) {
            Ok(e) => e,
            Err(_) => {
                let truncated = if data.len() > 200 { &data[..200] } else { data };
                tracing::warn!(
                    data = truncated,
                    "malformed Anthropic event, skipping"
                );
                return Ok(vec![]);
            }
        };

        let mut events = Vec::new();

        match event.r#type.as_str() {
            "message_start" => {
                if !self.started {
                    self.started = true;
                    let id = event
                        .message
                        .as_ref()
                        .and_then(|m| {
                            if m.id.is_empty() {
                                None
                            } else {
                                Some(m.id.clone())
                            }
                        });
                    events.push(CoreEvent::MessageStart {
                        id,
                        model: self.model_ref.clone(),
                    });
                }
            }
            "content_block_start" => {
                let idx = event.index.unwrap_or(0);
                self.current_block_index = Some(idx);

                if let Some(ref block) = event.content_block {
                    match block.r#type.as_str() {
                        "text" => {
                            self.current_block_kind = ContentKind::Text;
                            events.push(CoreEvent::ContentStart {
                                index: idx,
                                kind: ContentKind::Text,
                            });
                        }
                        "thinking" => {
                            self.current_block_kind = ContentKind::Thinking;
                            events.push(CoreEvent::ContentStart {
                                index: idx,
                                kind: ContentKind::Thinking,
                            });
                        }
                        "tool_use" => {
                            self.current_block_kind = ContentKind::ToolUse;
                            self.tool_blocks.push(idx);
                            self.tool_blocks_closed.push(false);
                            let id = block.id.clone().unwrap_or_default();
                            let name = block.name.clone().unwrap_or_default();
                            // Reverse-map sanitized tool names back to originals,
                            // same as the non-streaming decode path.
                            let original_name = desanitize_tool_name(&name).into_owned();
                            events.push(CoreEvent::ToolCallStart {
                                index: idx,
                                id,
                                name: original_name,
                            });
                        }
                        _ => {
                            // Unknown block type; emit generic ContentStart.
                            self.current_block_kind = ContentKind::Text;
                            events.push(CoreEvent::ContentStart {
                                index: idx,
                                kind: ContentKind::Text,
                            });
                        }
                    }
                }
            }
            "content_block_delta" => {
                let idx = event.index.unwrap_or(0);
                if let Some(ref delta) = event.delta {
                    match delta.r#type.as_deref().or(Some("")) {
                        Some("text_delta") => {
                            if let Some(ref text) = delta.text {
                                if !text.is_empty() {
                                    events.push(CoreEvent::TextDelta {
                                        index: idx,
                                        text: text.clone(),
                                    });
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(ref text) = delta.thinking {
                                if !text.is_empty() {
                                    events.push(CoreEvent::ThinkingDelta {
                                        index: idx,
                                        text: text.clone(),
                                    });
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(ref partial) = delta.partial_json {
                                if !partial.is_empty() {
                                    events.push(CoreEvent::ToolCallDelta {
                                        index: idx,
                                        args_delta: partial.clone(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let idx = event.index.unwrap_or(0);
                if self.current_block_kind == ContentKind::ToolUse {
                    events.push(CoreEvent::ToolCallStop { index: idx });
                    // Mark this tool block as closed so finish() won't re-emit.
                    let tool_idx = self
                        .tool_blocks
                        .iter()
                        .position(|&i| i == idx)
                        .unwrap_or(0);
                    if tool_idx < self.tool_blocks_closed.len() {
                        self.tool_blocks_closed[tool_idx] = true;
                    }
                }
                self.current_block_index = None;
                self.current_block_kind = ContentKind::Text;
            }
            "message_delta" => {
                if let Some(ref delta) = event.delta {
                    if let Some(ref reason) = delta.stop_reason {
                        if !reason.is_empty() && !self.stop_sent {
                            self.stop_sent = true;
                            let stop_reason = map_anthropic_stop_reason(reason);
                            let stop_sequence = delta.stop_sequence.clone();
                            events.push(CoreEvent::MessageStop {
                                stop_reason,
                                stop_sequence,
                            });
                        }
                    }
                }
                // Usage from message_delta.
                if let Some(ref usage) = event.usage {
                    events.push(CoreEvent::UsageDelta {
                        usage: build_anthropic_usage(
                            usage.input_tokens,
                            usage.output_tokens,
                            usage.cache_creation_input_tokens,
                            usage.cache_read_input_tokens,
                        ),
                    });
                }
            }
            "message_stop" => {
                if !self.stop_sent {
                    self.stop_sent = true;
                    events.push(CoreEvent::MessageStop {
                        stop_reason: if !self.tool_blocks.is_empty() {
                            StopReason::ToolUse
                        } else {
                            StopReason::EndTurn
                        },
                        stop_sequence: None,
                    });
                }
            }
            "ping" => {
                events.push(CoreEvent::Ping);
            }
            "error" => {
                if let Some(ref err) = event.error {
                    let kind = match err.r#type.as_str() {
                        "rate_limit_error" => {
                            llm_proxy_protocol::core::CoreStreamErrorKind::RateLimit
                        }
                        "authentication_error" => {
                            llm_proxy_protocol::core::CoreStreamErrorKind::Authentication
                        }
                        "invalid_request_error" => {
                            llm_proxy_protocol::core::CoreStreamErrorKind::InvalidRequest
                        }
                        "permission_error" => {
                            llm_proxy_protocol::core::CoreStreamErrorKind::Permission
                        }
                        _ => llm_proxy_protocol::core::CoreStreamErrorKind::Upstream,
                    };
                    events.push(CoreEvent::Error {
                        error: llm_proxy_protocol::core::CoreStreamError::new(
                            kind,
                            err.message.clone(),
                        ),
                    });
                }
            }
            _ => {
                // Unknown event type; skip.
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<CoreEvent>, ProviderError> {
        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            events.push(CoreEvent::MessageStart {
                id: None,
                model: self.model_ref.clone(),
            });
        }

        // Close any tool blocks that were NOT already closed by content_block_stop.
        for (i, &idx) in self.tool_blocks.iter().enumerate() {
            let closed = self.tool_blocks_closed.get(i).copied().unwrap_or(false);
            if !closed {
                events.push(CoreEvent::ToolCallStop { index: idx });
            }
        }

        if !self.stop_sent {
            self.stop_sent = true;
            events.push(CoreEvent::MessageStop {
                stop_reason: if !self.tool_blocks.is_empty() {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
                stop_sequence: None,
            });
        }

        Ok(events)
    }
}

// ---------------------------------------------------------------------------
// AnthropicAdapter impl
// ---------------------------------------------------------------------------

impl AnthropicAdapter {
    /// Encode a core request into an Anthropic Messages request.
    pub fn encode_request(
        &self,
        core: &CoreRequest,
        target: &ProviderAdapterTarget,
    ) -> Result<super::ProxyRequest, ProviderError> {
        // System prompt -- build as JSON array of SystemContentBlock.
        let system = if core.system.is_empty() {
            None
        } else {
            let blocks: Vec<serde_json::Value> = core
                .system
                .iter()
                .filter_map(|c| match c {
                    CoreContent::Text { text, cache } => {
                        if text.is_empty() {
                            return None;
                        }
                        let mut block = serde_json::json!({
                            "type": "text",
                            "text": text,
                        });
                        if let Some(cc) = cache {
                            block
                                .as_object_mut()
                                .expect("json! macro always produces an object")
                                .insert(
                                    "cache_control".to_owned(),
                                    serde_json::json!({"type": cc.r#type}),
                                );
                        }
                        Some(block)
                    }
                    other => {
                        tracing::warn!(
                            ?other,
                            "dropping non-Text system content block during Anthropic encode"
                        );
                        None
                    }
                })
                .collect();
            if blocks.is_empty() {
                None
            } else {
                Some(serde_json::Value::Array(blocks))
            }
        };

        // Messages -- build as JSON array.
        let mut messages = Vec::new();
        for msg in &core.messages {
            match msg.role {
                CoreRole::User => {
                    let content = encode_content_blocks(&msg.content);
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                CoreRole::Assistant => {
                    let content = encode_assistant_content(&msg.content);
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                    }));
                }
                CoreRole::System => {
                    // Merge system messages into the system field.
                    // Skip here; should have been handled by client adapter.
                }
                CoreRole::Tool => {
                    // Tool results come through user role in Anthropic.
                    let content = encode_content_blocks(&msg.content);
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                _ => {
                    // Handle future CoreRole variants.
                    let content = encode_content_blocks(&msg.content);
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            }
        }

        // Tools -- build as JSON array with name sanitization.
        let tools: Vec<serde_json::Value> = core
            .tools
            .iter()
            .map(|t| {
                // Coerce non-object input_schema to a default object schema.
                let schema = if !t.input_schema.is_object() {
                    serde_json::json!({"type": "object", "properties": {}})
                } else {
                    t.input_schema.clone()
                };
                let (sanitized_name, _changed) = sanitize_tool_name(&t.name);
                let mut tool = serde_json::json!({
                    "name": sanitized_name,
                    "input_schema": schema,
                });
                if let Some(ref desc) = t.description {
                    tool.as_object_mut()
                        .expect("json! macro always produces an object")
                        .insert(
                            "description".to_owned(),
                            serde_json::Value::String(desc.clone()),
                        );
                }
                tool
            })
            .collect();

        // Tool choice.
        let tool_choice = core.tool_choice.as_ref().map(|tc| match tc {
            CoreToolChoice::Auto => serde_json::json!({"type": "auto"}),
            CoreToolChoice::Any => serde_json::json!({"type": "any"}),
            CoreToolChoice::None => serde_json::json!({"type": "none"}),
            CoreToolChoice::Tool { name } => {
                let (sanitized, _) = sanitize_tool_name(name);
                serde_json::json!({
                    "type": "tool",
                    "name": sanitized
                })
            }
            CoreToolChoice::Raw(v) => v.clone(),
            _ => serde_json::json!(null),
        });

        // Build the full request as JSON to avoid #[non_exhaustive] struct literal issues.
        let mut req = serde_json::json!({
            "model": target.upstream_model,
            "max_tokens": core.sampling.max_tokens.unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
            "messages": messages,
        });

        let obj = req
            .as_object_mut()
            .expect("json! macro always produces an object");

        if let Some(sys) = system {
            obj.insert("system".to_owned(), sys);
        }
        if core.stream {
            obj.insert("stream".to_owned(), serde_json::json!(true));
        }
        if !tools.is_empty() {
            obj.insert("tools".to_owned(), serde_json::Value::Array(tools));
        }
        if let Some(temp) = core.sampling.temperature {
            obj.insert("temperature".to_owned(), serde_json::json!(temp));
        }
        if let Some(top_p) = core.sampling.top_p {
            obj.insert("top_p".to_owned(), serde_json::json!(top_p));
        }
        // Forward stop sequences.
        if let Some(ref stop) = core.sampling.stop {
            if !stop.is_empty() {
                obj.insert(
                    "stop_sequences".to_owned(),
                    serde_json::to_value(stop).unwrap_or(serde_json::Value::Null),
                );
            }
        }
        if let Some(ref user_id) = core.metadata.user_id {
            obj.insert("metadata".to_owned(), serde_json::json!({"user_id": user_id}));
        }
        if let Some(ref thinking) = core.sampling.thinking {
            obj.insert("thinking".to_owned(), thinking.clone());
        }
        // Forward reasoning_effort if present (Anthropic supports this in
        // extended thinking mode).
        if let Some(ref effort) = core.sampling.reasoning_effort {
            obj.insert("reasoning_effort".to_owned(), serde_json::Value::String(effort.clone()));
        }
        if let Some(tc) = tool_choice {
            obj.insert("tool_choice".to_owned(), tc);
        }

        let body = serde_json::to_vec(&req)?;
        let url = expand_url_template(&target.endpoint, target)?;

        Ok(build_proxy_request(body, target, core.stream, url))
    }

    /// Decode an Anthropic Messages response body.
    pub fn decode_response(
        &self,
        bytes: &[u8],
        target: &ProviderAdapterTarget,
    ) -> Result<CoreResponse, ProviderError> {
        let resp: MessageResponse = serde_json::from_slice(bytes)?;

        let mut content = Vec::new();

        for block in &resp.content {
            match block.r#type.as_str() {
                "text" => {
                    let text = block.text.clone().unwrap_or_default();
                    content.push(CoreContent::Text {
                        text,
                        cache: None,
                    });
                }
                "thinking" => {
                    let thinking = block.thinking.clone().unwrap_or_default();
                    content.push(CoreContent::Thinking {
                        text: thinking,
                        signature: block.signature.clone(),
                    });
                }
                "redacted_thinking" => {
                    content.push(CoreContent::RedactedThinking {
                        data: serde_json::json!(block.data.clone().unwrap_or_default()),
                    });
                }
                "tool_use" => {
                    let input = block
                        .input
                        .clone()
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    // Reverse-map sanitized tool names back to originals.
                    let name = block.name.clone().unwrap_or_default();
                    let original_name = desanitize_tool_name(&name).into_owned();
                    content.push(CoreContent::ToolUse {
                        id: block.id.clone().unwrap_or_default(),
                        name: original_name,
                        input,
                    });
                }
                _ => {
                    // Unknown block type; store as text for safety.
                    let text = block.text.clone().unwrap_or_default();
                    if !text.is_empty() {
                        content.push(CoreContent::Text {
                            text,
                            cache: None,
                        });
                    }
                }
            }
        }

        // Guarantee at least one content block.
        if content.is_empty() {
            content.push(CoreContent::Text {
                text: String::new(),
                cache: None,
            });
        }

        let stop_reason = resp
            .stop_reason
            .as_deref()
            .map(map_anthropic_stop_reason)
            .unwrap_or(StopReason::Unknown);

        let usage = build_anthropic_usage(
            resp.usage.input_tokens,
            resp.usage.output_tokens,
            resp.usage.cache_creation_input_tokens,
            resp.usage.cache_read_input_tokens,
        );

        Ok(CoreResponse {
            id: if resp.id.is_empty() {
                None
            } else {
                Some(resp.id)
            },
            model: response_model_ref(target),
            content,
            stop_reason,
            stop_sequence: resp.stop_sequence,
            usage,
            provider_meta: serde_json::Map::new(),
        })
    }

    /// Create a new stream decoder.
    pub fn new_stream_decoder(
        &self,
        target: &ProviderAdapterTarget,
    ) -> Box<dyn ProviderStreamDecoder + Send> {
        Box::new(AnthropicStreamDecoder {
            model_ref: response_model_ref(target),
            started: false,
            current_block_index: None,
            current_block_kind: ContentKind::Text,
            tool_blocks: Vec::new(),
            tool_blocks_closed: Vec::new(),
            stop_sent: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map an Anthropic stop reason to a core StopReason.
fn map_anthropic_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "stop_sequence" => StopReason::StopSequence,
        "refusal" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

/// Build usage from Anthropic-style usage info.
fn build_anthropic_usage(
    input_tokens: i32,
    output_tokens: i32,
    cache_creation: Option<i32>,
    cache_read: Option<i32>,
) -> llm_proxy_protocol::core::Usage {
    llm_proxy_protocol::core::Usage {
        input_tokens,
        output_tokens,
        reasoning_tokens: None,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        provenance: UsageProvenance::ProviderReported,
    }
}

/// Encode content blocks for a user message.
fn encode_content_blocks(content: &[CoreContent]) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = content
        .iter()
        .filter_map(|c| match c {
            CoreContent::Text { text, .. } => {
                if text.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "type": "text",
                    "text": text,
                }))
            }
            CoreContent::ToolResult {
                tool_use_id,
                content: result_content,
                is_error,
            } => {
                let result_text = result_content
                    .iter()
                    .filter_map(|c| match c {
                        CoreContent::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                let sanitized_id = sanitize_tool_use_id(tool_use_id);
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": sanitized_id,
                    "content": result_text,
                });
                if *is_error {
                    block
                        .as_object_mut()
                        .expect("json! macro always produces an object")
                        .insert(
                            "is_error".to_owned(),
                            serde_json::json!(true),
                        );
                }
                Some(block)
            }
            CoreContent::Image { source } => {
                Some(serde_json::json!({
                    "type": "image",
                    "source": source,
                }))
            }
            other => {
                tracing::warn!(
                    ?other,
                    "dropping unsupported content block during Anthropic encode"
                );
                None
            }
        })
        .collect();

    serde_json::Value::Array(blocks)
}

/// Encode content blocks for an assistant message.
fn encode_assistant_content(content: &[CoreContent]) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = content
        .iter()
        .filter_map(|c| match c {
            CoreContent::Text { text, .. } => {
                if text.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "type": "text",
                    "text": text,
                }))
            }
            CoreContent::Thinking { text, signature } => {
                if text.is_empty() {
                    return None;
                }
                let mut block = serde_json::json!({
                    "type": "thinking",
                    "thinking": text,
                });
                if let Some(sig) = signature {
                    block
                        .as_object_mut()
                        .expect("json! macro always produces an object")
                        .insert(
                            "signature".to_owned(),
                            serde_json::Value::String(sig.clone()),
                        );
                }
                Some(block)
            }
            CoreContent::ToolUse { id, name, input } => {
                let (sanitized_name, _) = sanitize_tool_name(name);
                let sanitized_id = sanitize_tool_use_id(id);
                Some(serde_json::json!({
                    "type": "tool_use",
                    "id": sanitized_id,
                    "name": sanitized_name,
                    "input": input,
                }))
            }
            other => {
                tracing::warn!(
                    ?other,
                    "dropping unsupported content block during Anthropic assistant encode"
                );
                None
            }
        })
        .collect();

    serde_json::Value::Array(blocks)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use llm_proxy_protocol::core::{CoreMessage, CoreRequest, CoreTool, SamplingOptions};

    fn make_target() -> ProviderAdapterTarget {
        ProviderAdapterTarget {
            provider_name: "test-anthropic".into(),
            adapter_name: "anthropic".into(),
            protocol: super::super::ProviderProtocol::AnthropicMessages,
            endpoint: "https://api.anthropic.com/v1/messages".into(),
            auth_style: llm_proxy_core::AuthStyle::XApiKey,
            api_key: "test-key".into(),
            requested_model: "claude-sonnet-4-20250514".into(),
            upstream_model: "claude-sonnet-4-20250514".into(),
        }
    }

    fn make_core_request(messages: Vec<CoreMessage>) -> CoreRequest {
        CoreRequest {
            model: ModelRef {
                requested: "claude-sonnet-4-20250514".into(),
                upstream: None,
            },
            system: vec![],
            messages,
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions::default(),
            stream: false,
            metadata: Default::default(),
            provider_hints: Default::default(),
        }
    }

    // -- Tool name sanitization tests ----------------------------------------

    #[test]
    fn sanitize_tool_name_valid() {
        let (name, changed) = sanitize_tool_name("get_weather");
        assert_eq!(name, "get_weather");
        assert!(!changed);
        let (name, changed) = sanitize_tool_name("my-tool-123");
        assert_eq!(name, "my-tool-123");
        assert!(!changed);
    }

    #[test]
    fn sanitize_tool_name_replaces_dots_collision_safe() {
        // "my.tool.name" should NOT collide with "my_tool_name".
        let (name1, changed1) = sanitize_tool_name("my.tool.name");
        let (name2, changed2) = sanitize_tool_name("my_tool_name");
        assert!(changed1);
        assert!(!changed2);
        assert_ne!(name1, name2, "collision-safe: different names must produce different sanitized names");
        assert!(name1.contains("0x2e"), "dot should be encoded as _0x2e_");
    }

    #[test]
    fn sanitize_tool_name_reversible() {
        let original = "my.tool+name";
        let (sanitized, _) = sanitize_tool_name(original);
        let restored = desanitize_tool_name(&sanitized);
        assert_eq!(restored, original);
    }

    #[test]
    fn sanitize_tool_name_truncates_long() {
        let long_name = "a".repeat(200);
        let (result, _) = sanitize_tool_name(&long_name);
        assert_eq!(result.len(), 128);
    }

    #[test]
    fn sanitize_tool_name_empty_becomes_tool() {
        let (name, _) = sanitize_tool_name("");
        assert_eq!(name, "tool");
    }

    #[test]
    fn sanitize_tool_name_special_chars() {
        let (name, changed) = sanitize_tool_name("get weather!@#");
        assert!(changed);
        // Spaces and special chars should be encoded, not simply replaced with _.
        assert!(name.contains("0x"));
    }

    // -- Tool use ID sanitization tests ----------------------------------------

    #[test]
    fn sanitize_tool_use_id_valid() {
        assert_eq!(sanitize_tool_use_id("toolu_123"), "toolu_123");
    }

    #[test]
    fn sanitize_tool_use_id_replaces_dashes() {
        assert_eq!(sanitize_tool_use_id("call-123"), "call_123");
    }

    #[test]
    fn sanitize_tool_use_id_empty_becomes_default() {
        assert_eq!(sanitize_tool_use_id(""), "tool_result");
    }

    // -- Encode tests --------------------------------------------------------

    #[test]
    fn encode_text_request() {
        let core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "Hello".into(),
                cache: None,
            }],
        }]);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn encode_system_prompt() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.system = vec![CoreContent::Text {
            text: "You are helpful".into(),
            cache: None,
        }];
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert!(body.get("system").is_some());
    }

    #[test]
    fn encode_tool_declarations_sanitized() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.tools = vec![CoreTool {
            name: "get.weather".into(),
            description: Some("Get weather".into()),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        // Dots are encoded collision-safe as _0x2e_
        assert!(tools[0]["name"].as_str().unwrap().contains("0x2e"));
    }

    #[test]
    fn encode_tool_choice() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.tool_choice = Some(CoreToolChoice::Auto);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body["tool_choice"], serde_json::json!({"type": "auto"}));
    }

    #[test]
    fn encode_stream_flag() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.stream = true;
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body["stream"], true);
        assert!(proxy_req.stream);
    }

    #[test]
    fn encode_metadata_user_id() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.metadata.user_id = Some("user-42".into());
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert!(body.get("metadata").is_some());
        assert_eq!(body["metadata"]["user_id"], "user-42");
    }

    #[test]
    fn encode_thinking_passthrough() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.sampling.thinking = Some(serde_json::json!({"type": "enabled", "budget_tokens": 5000}));
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert!(body.get("thinking").is_some());
        assert_eq!(body["thinking"]["budget_tokens"], 5000);
    }

    // -- Decode tests --------------------------------------------------------

    #[test]
    fn decode_text_response() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hello world"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 10,
                "cache_read_input_tokens": 20,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.id, Some("msg_test".to_owned()));
        assert_eq!(core_resp.model.requested, "claude-sonnet-4-20250514");
        assert_eq!(core_resp.stop_reason, StopReason::EndTurn);
        assert_eq!(core_resp.usage.input_tokens, 100);
        assert_eq!(core_resp.usage.output_tokens, 50);
    }

    #[test]
    fn decode_tool_use_response() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "SF"}}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.stop_reason, StopReason::ToolUse);
        match &core_resp.content[0] {
            CoreContent::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "SF");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn decode_thinking_response() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hmm..."},
                {"type": "text", "text": "answer"}
            ],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.content.len(), 2);
        match &core_resp.content[0] {
            CoreContent::Thinking { text, signature } => {
                assert_eq!(text, "hmm...");
                assert!(signature.is_none());
            }
            _ => panic!("expected Thinking"),
        }
    }

    #[test]
    fn decode_preserves_requested_model() {
        let mut target = make_target();
        target.upstream_model = "claude-sonnet-4-20250514".into();
        target.requested_model = "my-claude".into();

        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.model.requested, "my-claude");
        assert_eq!(
            core_resp.model.upstream.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    // -- Streaming tests -----------------------------------------------------

    #[test]
    fn stream_text_decoding() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#),
            make_frame(r#"{"type":"content_block_stop","index":0}"#),
            make_frame(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":0,"output_tokens":10}}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }

        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::MessageStart { .. })));
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::ContentStart { .. })));
        assert!(all_events.iter().any(|e| matches!(
            e,
            CoreEvent::TextDelta { text, .. } if text == "Hello"
        )));
        assert!(all_events.iter().any(|e| matches!(
            e,
            CoreEvent::TextDelta { text, .. } if text == " world"
        )));
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })));
    }

    #[test]
    fn stream_tool_call_decoding() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather"}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"SF\"}"}}"#),
            make_frame(r#"{"type":"content_block_stop","index":0}"#),
            make_frame(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"input_tokens":0,"output_tokens":20}}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }

        assert!(all_events.iter().any(|e| matches!(
            e,
            CoreEvent::ToolCallStart { name, .. } if name == "get_weather"
        )));
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::ToolCallDelta { .. })));
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::ToolCallStop { .. })));
        assert!(all_events.iter().any(|e| matches!(
            e,
            CoreEvent::MessageStop { stop_reason: StopReason::ToolUse, .. }
        )));
    }

    #[test]
    fn stream_ping_event() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame(r#"{"type":"ping"}"#);
        let events = decoder.decode_frame(&frame).unwrap();
        assert!(events.iter().any(|e| matches!(e, CoreEvent::Ping)));
    }

    #[test]
    fn stream_error_event() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame(r#"{"type":"error","error":{"type":"overloaded_error","message":"Too many requests"}}"#);
        let events = decoder.decode_frame(&frame).unwrap();
        assert!(events.iter().any(|e| matches!(e, CoreEvent::Error { .. })));
    }

    #[test]
    fn stream_finish_emits_lifecycle() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let events = decoder.finish().unwrap();
        assert!(events.iter().any(|e| matches!(e, CoreEvent::MessageStart { .. })));
        assert!(events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })));
    }

    #[test]
    fn stream_thinking_decoding() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think..."}}"#),
            make_frame(r#"{"type":"content_block_stop","index":0}"#),
            make_frame(r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            make_frame(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}"#),
            make_frame(r#"{"type":"content_block_stop","index":1}"#),
            make_frame(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":0,"output_tokens":20}}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }

        assert!(all_events.iter().any(|e| matches!(
            e,
            CoreEvent::ThinkingDelta { text, .. } if text == "Let me think..."
        )));
        assert!(all_events.iter().any(|e| matches!(
            e,
            CoreEvent::TextDelta { text, .. } if text == "answer"
        )));
    }

    // -- Stop reason mapping tests -------------------------------------------

    #[test]
    fn anthropic_stop_reason_mappings() {
        assert_eq!(map_anthropic_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(map_anthropic_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_anthropic_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_anthropic_stop_reason("stop_sequence"), StopReason::StopSequence);
        assert_eq!(map_anthropic_stop_reason("refusal"), StopReason::Refusal);
        assert_eq!(map_anthropic_stop_reason("unknown"), StopReason::Unknown);
    }

    // -- Missing tests: encode stop sequences, input_schema coercion ----------

    #[test]
    fn encode_stop_sequences() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.sampling.stop = Some(vec!["END".into(), "STOP".into()]);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(
            body["stop_sequences"],
            serde_json::json!(["END", "STOP"])
        );
    }

    #[test]
    fn encode_input_schema_null_coerced_to_object() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.tools = vec![CoreTool {
            name: "my_tool".into(),
            description: Some("test".into()),
            input_schema: serde_json::Value::Null,
        }];
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let schema = &body["tools"].as_array().unwrap()[0]["input_schema"];
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn encode_input_schema_string_coerced_to_object() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.tools = vec![CoreTool {
            name: "my_tool".into(),
            description: Some("test".into()),
            input_schema: serde_json::json!("not an object"),
        }];
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let schema = &body["tools"].as_array().unwrap()[0]["input_schema"];
        assert_eq!(schema["type"], "object");
    }

    // -- Missing tests: decode redacted thinking, stop_sequence, malformed frame --

    #[test]
    fn decode_redacted_thinking_response() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "redacted_thinking", "data": "opaque"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        match &core_resp.content[0] {
            CoreContent::RedactedThinking { .. } => {}
            other => panic!("expected RedactedThinking, got {:?}", other),
        }
    }

    #[test]
    fn decode_stop_sequence_propagated() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "stop_sequence",
            "stop_sequence": "END",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.stop_reason, StopReason::StopSequence);
        assert_eq!(core_resp.stop_sequence, Some("END".to_owned()));
    }

    #[test]
    fn stream_malformed_frame_skipped() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame("{not valid json}");
        let events = decoder.decode_frame(&frame).unwrap();
        assert!(events.is_empty());
    }

    // -- Missing test: usage-only chunk classification ------------------------

    #[test]
    fn stream_usage_only_chunk() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        // Send a message_delta with usage but no text content.
        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":0,"output_tokens":20}}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }

        // Usage should NOT be dropped just because there was no text.
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::UsageDelta { .. })));
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })));
    }

    // -- Missing test: full lifecycle event ordering --------------------------

    #[test]
    fn stream_full_lifecycle_ordering() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#),
            make_frame(r#"{"type":"content_block_stop","index":0}"#),
            make_frame(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":0,"output_tokens":5}}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }

        // Verify ordering: MessageStart < ContentStart < TextDelta < MessageStop.
        let msg_start_idx = all_events.iter().position(|e| matches!(e, CoreEvent::MessageStart { .. })).unwrap();
        let content_start_idx = all_events.iter().position(|e| matches!(e, CoreEvent::ContentStart { .. })).unwrap();
        let text_delta_idx = all_events.iter().position(|e| matches!(e, CoreEvent::TextDelta { .. })).unwrap();
        let msg_stop_idx = all_events.iter().position(|e| matches!(e, CoreEvent::MessageStop { .. })).unwrap();

        assert!(msg_start_idx < content_start_idx, "MessageStart must precede ContentStart");
        assert!(content_start_idx < text_delta_idx, "ContentStart must precede TextDelta");
        assert!(text_delta_idx < msg_stop_idx, "TextDelta must precede MessageStop");
    }

    // -- Additional missing tests ----------------------------------------------

    #[test]
    fn encode_tool_result_as_tool_result_block() {
        let core = make_core_request(vec![CoreMessage {
            role: CoreRole::Tool,
            content: vec![CoreContent::ToolResult {
                tool_use_id: "toolu_123".into(),
                content: vec![CoreContent::Text {
                    text: "72F sunny".into(),
                    cache: None,
                }],
                is_error: false,
            }],
        }]);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
        assert_eq!(content[0]["content"], "72F sunny");
    }

    #[test]
    fn encode_tool_result_error_flag() {
        let core = make_core_request(vec![CoreMessage {
            role: CoreRole::Tool,
            content: vec![CoreContent::ToolResult {
                tool_use_id: "toolu_err".into(),
                content: vec![CoreContent::Text {
                    text: "error occurred".into(),
                    cache: None,
                }],
                is_error: true,
            }],
        }]);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let content = &body["messages"].as_array().unwrap()[0]["content"].as_array().unwrap()[0];
        assert_eq!(content["is_error"], true);
    }

    #[test]
    fn encode_cache_control_forwarded() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.system = vec![CoreContent::Text {
            text: "You are helpful".into(),
            cache: Some(llm_proxy_protocol::core::CacheControl {
                r#type: "ephemeral".to_owned().into(),
            }),
        }];
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn encode_image_content_forwarded() {
        let core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![
                CoreContent::Text {
                    text: "What is this?".into(),
                    cache: None,
                },
                CoreContent::Image {
                    source: serde_json::json!({
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "iVBOR..."
                    }),
                },
            ],
        }]);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let content = body["messages"].as_array().unwrap()[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["type"], "image");
    }

    #[test]
    fn sanitize_tool_use_id_long_truncated() {
        let long_id = "a".repeat(300);
        let sanitized = sanitize_tool_use_id(&long_id);
        assert!(sanitized.len() <= 256);
    }

    #[test]
    fn desanitize_no_pattern_returns_original() {
        let name = "get_weather";
        assert_eq!(desanitize_tool_name(name), name);
    }

    #[test]
    fn desanitize_roundtrip_unicode() {
        let original = "tool.中文";
        let (sanitized, _) = sanitize_tool_name(original);
        let restored = desanitize_tool_name(&sanitized);
        assert_eq!(restored, original);
    }

    #[test]
    fn encode_reasoning_effort_forwarded() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.sampling.reasoning_effort = Some("high".into());
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn encode_tool_choice_tool_sanitized() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.tool_choice = Some(CoreToolChoice::Tool {
            name: "get.weather".into(),
        });
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        let tc_name = body["tool_choice"]["name"].as_str().unwrap();
        assert!(tc_name.contains("0x2e"), "tool name in tool_choice must be sanitized");
    }

    #[test]
    fn decode_empty_content_gets_default_text() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.content.len(), 1);
        assert_eq!(core_resp.content[0], CoreContent::Text { text: String::new(), cache: None });
    }

    #[test]
    fn stream_unknown_event_skipped() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame(r#"{"type":"some_new_event","data":"whatever"}"#);
        let events = decoder.decode_frame(&frame).unwrap();
        // Unknown event types produce no events (they are skipped).
        assert!(events.is_empty());
    }

    #[test]
    fn stream_redacted_thinking_decoded() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "redacted_thinking", "data": "opaque_blob"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = AnthropicAdapter::new();
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        match &core_resp.content[0] {
            CoreContent::RedactedThinking { .. } => {}
            other => panic!("expected RedactedThinking, got {:?}", other),
        }
    }

    // -- Additional tests: sentinel, false-positive, upstream model, malformed, etc --

    #[test]
    fn desanitize_sentinel_prevents_false_positive() {
        // A tool name that naturally contains _0xHH_ should NOT be decoded.
        let natural_name = "parse_0xff_value";
        // This name was never sanitized, so it should be returned as-is.
        assert_eq!(desanitize_tool_name(natural_name), natural_name);
    }

    #[test]
    fn sanitize_desanitize_roundtrip_with_sentinel() {
        let original = "my.tool+name";
        let (sanitized, changed) = sanitize_tool_name(original);
        assert!(changed, "should have changed");
        assert!(sanitized.starts_with("__llmp_"), "sanitized name should have sentinel");
        let restored = desanitize_tool_name(&sanitized);
        assert_eq!(restored, original);
    }

    #[test]
    fn sanitize_no_change_no_sentinel() {
        let original = "get_weather";
        let (sanitized, changed) = sanitize_tool_name(original);
        assert!(!changed, "should not have changed");
        assert!(!sanitized.starts_with("__llmp_"), "unchanged name should not have sentinel");
        assert_eq!(sanitized, original);
    }

    #[test]
    fn encode_uses_upstream_model() {
        let mut target = make_target();
        target.upstream_model = "claude-sonnet-4-20250514-alias".into();
        let core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        let adapter = AnthropicAdapter::new();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-20250514-alias");
    }

    #[test]
    fn decode_malformed_json_returns_error() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let result = adapter.decode_response(b"not valid json {{{", &target);
        assert!(result.is_err(), "malformed JSON should produce an error");
    }

    #[test]
    fn decode_empty_bytes_returns_error() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let result = adapter.decode_response(b"", &target);
        assert!(result.is_err(), "empty bytes should produce an error");
    }

    #[test]
    fn encode_empty_messages() {
        let core = make_core_request(vec![]);
        let adapter = AnthropicAdapter::new();
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&proxy_req.body).unwrap();
        // Should produce a valid request with empty messages array.
        assert_eq!(body["messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn stream_error_maps_rate_limit() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame(
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Too many requests"}}"#,
        );
        let events = decoder.decode_frame(&frame).unwrap();
        let error_event = events.iter().find(|e| matches!(e, CoreEvent::Error { .. }));
        assert!(error_event.is_some());
        if let CoreEvent::Error { error } = error_event.unwrap() {
            assert_eq!(error.kind, llm_proxy_protocol::core::CoreStreamErrorKind::RateLimit);
        }
    }

    #[test]
    fn stream_error_maps_authentication() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame(
            r#"{"type":"error","error":{"type":"authentication_error","message":"Invalid API key"}}"#,
        );
        let events = decoder.decode_frame(&frame).unwrap();
        let error_event = events.iter().find(|e| matches!(e, CoreEvent::Error { .. }));
        assert!(error_event.is_some());
        if let CoreEvent::Error { error } = error_event.unwrap() {
            assert_eq!(error.kind, llm_proxy_protocol::core::CoreStreamErrorKind::Authentication);
        }
    }

    #[test]
    fn stream_finish_reason_only_chunk() {
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        // A chunk that only has message_stop with no prior content.
        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"message_stop"}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }

        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::MessageStart { .. })));
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })),
            "message_stop event must not be silently dropped");
    }

    #[test]
    fn stream_no_duplicate_tool_call_stop() {
        // Verify that finish() does NOT emit duplicate ToolCallStop for tool
        // blocks that already received ToolCallStop during decode_frame().
        let target = make_target();
        let adapter = AnthropicAdapter::new();
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
            make_frame(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather"}}"#),
            make_frame(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#),
            make_frame(r#"{"type":"content_block_stop","index":0}"#),
        ];

        let mut all_events = Vec::new();
        for frame in &frames {
            all_events.extend(decoder.decode_frame(frame).unwrap());
        }
        all_events.extend(decoder.finish().unwrap());

        // Count ToolCallStop events -- should be exactly 1.
        let tool_call_stop_count = all_events
            .iter()
            .filter(|e| matches!(e, CoreEvent::ToolCallStop { .. }))
            .count();
        assert_eq!(tool_call_stop_count, 1, "ToolCallStop should be emitted exactly once");
    }

    // -- Source guard ---------------------------------------------------------

    #[test]
    fn adapter_source_no_forbidden_imports() {
        let source = include_str!("anthropic.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);
        assert!(
            !prod.contains("llm_proxy_protocol::client"),
            "adapter must not import client protocol types"
        );
        assert!(
            !prod.contains("llm_proxy_server"),
            "adapter must not import server crate"
        );
    }

    // -- Helpers --------------------------------------------------------------

    fn make_frame(data: &str) -> SseFrame {
        SseFrame {
            event: None,
            id: None,
            data: data.to_owned(),
        }
    }
}
