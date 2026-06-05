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
// Tool name sanitization
// ---------------------------------------------------------------------------

/// Anthropic requires tool names matching `^[a-zA-Z0-9_-]{1,128}$`.
/// This regex matches characters that are NOT in the allowed set.
static INVALID_TOOL_NAME_CHAR: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^a-zA-Z0-9_-]").expect("valid regex"));

/// Sanitize a tool name for the Anthropic API.
///
/// Replaces disallowed characters with underscores and truncates to 128 chars.
fn sanitize_tool_name(name: &str) -> String {
    let sanitized: String = INVALID_TOOL_NAME_CHAR
        .replace_all(name, "_")
        .into_owned();
    let truncated = if sanitized.len() > 128 {
        let mut end = 128;
        while !sanitized.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &sanitized[..end]
    } else {
        &sanitized
    };
    // Guarantee non-empty.
    if truncated.is_empty() {
        "tool".to_owned()
    } else {
        truncated.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Adapter for the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicAdapter {
    _private: (),
}

impl AnthropicAdapter {
    /// Create a new Anthropic adapter.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

// ---------------------------------------------------------------------------
// Stream decoder
// ---------------------------------------------------------------------------

/// Stateful stream decoder for Anthropic SSE frames.
#[derive(Debug)]
pub struct AnthropicStreamDecoder {
    model_ref: ModelRef,
    started: bool,
    current_block_index: Option<usize>,
    current_block_kind: ContentKind,
    tool_blocks: Vec<usize>,
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
                tracing::warn!(data, "malformed Anthropic event, skipping");
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
                            let id = block.id.clone().unwrap_or_default();
                            let name = block.name.clone().unwrap_or_default();
                            events.push(CoreEvent::ToolCallStart {
                                index: idx,
                                id,
                                name,
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
                    events.push(CoreEvent::Error {
                        error: llm_proxy_protocol::core::CoreStreamError::new(
                            llm_proxy_protocol::core::CoreStreamErrorKind::Upstream,
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

        // Close any open tool blocks.
        for &idx in &self.tool_blocks {
            events.push(CoreEvent::ToolCallStop { index: idx });
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
                            block.as_object_mut().unwrap().insert(
                                "cache_control".to_owned(),
                                serde_json::json!({"type": cc.r#type}),
                            );
                        }
                        Some(block)
                    }
                    _ => None,
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

        // Tools -- build as JSON array.
        let tools: Vec<serde_json::Value> = core
            .tools
            .iter()
            .map(|t| {
                let schema = if t.input_schema.is_null() {
                    serde_json::json!({"type": "object", "properties": {}})
                } else {
                    t.input_schema.clone()
                };
                let mut tool = serde_json::json!({
                    "name": sanitize_tool_name(&t.name),
                    "input_schema": schema,
                });
                if let Some(ref desc) = t.description {
                    tool.as_object_mut().unwrap().insert(
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
            CoreToolChoice::Tool { name } => serde_json::json!({
                "type": "tool",
                "name": sanitize_tool_name(name)
            }),
            CoreToolChoice::Raw(v) => v.clone(),
            _ => serde_json::json!(null),
        });

        // Build the full request as JSON to avoid #[non_exhaustive] struct literal issues.
        let mut req = serde_json::json!({
            "model": target.upstream_model,
            "max_tokens": core.sampling.max_tokens.unwrap_or(4096),
            "messages": messages,
        });

        let obj = req.as_object_mut().unwrap();

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
        if let Some(ref user_id) = core.metadata.user_id {
            obj.insert("metadata".to_owned(), serde_json::json!({"user_id": user_id}));
        }
        if let Some(ref thinking) = core.sampling.thinking {
            obj.insert("thinking".to_owned(), thinking.clone());
        }
        if let Some(tc) = tool_choice {
            obj.insert("tool_choice".to_owned(), tc);
        }

        let body = serde_json::to_vec(&req)?;
        let url = expand_url_template(&target.endpoint, target);

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
                    content.push(CoreContent::ToolUse {
                        id: block.id.clone().unwrap_or_default(),
                        name: block.name.clone().unwrap_or_default(),
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
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": result_text,
                });
                if *is_error {
                    block.as_object_mut().unwrap().insert(
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
            _ => None,
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
                    block.as_object_mut().unwrap().insert(
                        "signature".to_owned(),
                        serde_json::Value::String(sig.clone()),
                    );
                }
                Some(block)
            }
            CoreContent::ToolUse { id, name, input } => Some(serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": sanitize_tool_name(name),
                "input": input,
            })),
            _ => None,
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
        assert_eq!(sanitize_tool_name("get_weather"), "get_weather");
        assert_eq!(sanitize_tool_name("my-tool-123"), "my-tool-123");
    }

    #[test]
    fn sanitize_tool_name_replaces_dots() {
        assert_eq!(sanitize_tool_name("my.tool.name"), "my_tool_name");
    }

    #[test]
    fn sanitize_tool_name_truncates_long() {
        let long_name = "a".repeat(200);
        let result = sanitize_tool_name(&long_name);
        assert_eq!(result.len(), 128);
    }

    #[test]
    fn sanitize_tool_name_empty_becomes_tool() {
        assert_eq!(sanitize_tool_name(""), "tool");
    }

    #[test]
    fn sanitize_tool_name_special_chars() {
        assert_eq!(sanitize_tool_name("get weather!@#"), "get_weather___");
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
        assert_eq!(tools[0]["name"], "get_weather");
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
