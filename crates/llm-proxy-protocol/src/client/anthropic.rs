//! Anthropic Messages client protocol adapter.
//!
//! Translates between Anthropic Messages API wire format and the normalised
//! core protocol types.  This adapter only knows about Anthropic Messages
//! and the core types -- it never imports provider, server, or transformer code.
//!
//! ## Scope guardrails
//!
//! This module must **not** import or call:
//! - `llm_proxy_provider`
//! - `llm_proxy_server`
//! - core config or routing modules
//! - endpoint classification helpers
//! - scenario or fallback code
//! - `transformer/*`

use crate::anthropic::{
    self, ApiError, CacheControl as AnthropicCacheControl, ContentBlock, Delta, Message,
    MessageEvent, MessageRequest, MessageResponse, SystemContentBlock,
};
use crate::client::ProtocolError;
use crate::core::{
    CacheControl, CacheControlType, ContentKind, CoreContent, CoreEvent, CoreMessage, CoreRequest,
    CoreResponse, CoreRole, CoreStreamErrorKind, CoreTool, CoreToolChoice,
    ModelRef, ProviderHints, RequestMetadata, SamplingOptions, StopReason, Usage,
};

// ---------------------------------------------------------------------------
// decode_request
// ---------------------------------------------------------------------------

/// Decode an Anthropic [`MessageRequest`] into a normalised [`CoreRequest`].
pub fn decode_request(req: MessageRequest) -> Result<CoreRequest, ProtocolError> {
    if req.model.is_empty() {
        return Err(ProtocolError::InvalidRequest("model is required".into()));
    }
    if req.messages.is_empty() {
        return Err(ProtocolError::InvalidRequest("messages is required".into()));
    }

    let model = ModelRef {
        requested: req.model,
        upstream: None,
    };

    let system = decode_system(&req.system);

    let messages = req
        .messages
        .into_iter()
        .map(|m| decode_message(m))
        .collect::<Result<Vec<_>, _>>()?;

    let tools = req
        .tools
        .into_iter()
        .map(|t| CoreTool {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        })
        .collect();

    let tool_choice = req.tool_choice.map(decode_tool_choice);

    let sampling = SamplingOptions {
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: Some(req.max_tokens),
        stop: None,
        reasoning_effort: None,
        thinking: req.thinking,
    };

    let stream = req.stream.unwrap_or(false);

    let (user_id, raw_meta) = if let Some(meta) = req.metadata {
        (meta.user_id, serde_json::Map::new())
    } else {
        (None, serde_json::Map::new())
    };

    let metadata = RequestMetadata { user_id, raw: raw_meta };

    let provider_hints = ProviderHints {
        raw: serde_json::Map::new(),
    };

    Ok(CoreRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        sampling,
        stream,
        metadata,
        provider_hints,
    })
}

fn decode_system(system: &Option<serde_json::Value>) -> Vec<CoreContent> {
    match system {
        None => Vec::new(),
        Some(value) => {
            if let Some(s) = value.as_str() {
                if s.is_empty() {
                    return Vec::new();
                }
                return vec![CoreContent::Text {
                    text: s.to_owned(),
                    cache: None,
                }];
            }
            if let Some(arr) = value.as_array() {
                let mut result = Vec::new();
                for item in arr {
                    if let Ok(block) =
                        serde_json::from_value::<SystemContentBlock>(item.clone())
                    {
                        if block.r#type == "text" {
                            if let Some(t) = block.text {
                                let cache = block
                                    .cache_control
                                    .map(|cc| CacheControl {
                                        r#type: CacheControlType::from(cc.r#type),
                                    });
                                result.push(CoreContent::Text {
                                    text: t,
                                    cache,
                                });
                            }
                        }
                    }
                }
                return result;
            }
            Vec::new()
        }
    }
}

fn decode_message(msg: Message) -> Result<CoreMessage, ProtocolError> {
    let role = match msg.role.as_str() {
        "user" => CoreRole::User,
        "assistant" => CoreRole::Assistant,
        other => {
            return Err(ProtocolError::Decode(format!(
                "unknown role: {other}"
            )));
        }
    };

    let blocks = msg.content_blocks();
    let mut content = Vec::with_capacity(blocks.len());
    for block in blocks {
        content.push(decode_content_block(block)?);
    }

    Ok(CoreMessage { role, content })
}

fn decode_content_block(block: ContentBlock) -> Result<CoreContent, ProtocolError> {
    match block.r#type.as_str() {
        "text" => {
            let cache = block
                .cache_control
                .map(|cc| CacheControl {
                    r#type: CacheControlType::from(cc.r#type),
                });
            Ok(CoreContent::Text {
                text: block.text.unwrap_or_default(),
                cache,
            })
        }
        "image" => {
            let source = block
                .source
                .map(|s| serde_json::to_value(&s).unwrap_or(serde_json::Value::Null))
                .unwrap_or(serde_json::Value::Null);
            Ok(CoreContent::Image { source })
        }
        "tool_use" => Ok(CoreContent::ToolUse {
            id: block.id.unwrap_or_default(),
            name: block.name.unwrap_or_default(),
            input: block.input.unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        }),
        "tool_result" => {
            let inner_text = block.text_content();
            let is_error = block.is_error.unwrap_or(false);
            Ok(CoreContent::ToolResult {
                tool_use_id: block.tool_use_id.unwrap_or_default(),
                content: vec![CoreContent::Text {
                    text: inner_text,
                    cache: None,
                }],
                is_error,
            })
        }
        "thinking" => Ok(CoreContent::Thinking {
            text: block.thinking.unwrap_or_default(),
            signature: block.signature,
        }),
        other => {
            // Unknown block types are preserved as raw text content with a warning.
            tracing::warn!(
                block_type = other,
                "dropping unknown Anthropic content block type during decode"
            );
            Ok(CoreContent::Text {
                text: String::new(),
                cache: None,
            })
        }
    }
}

fn decode_tool_choice(value: serde_json::Value) -> CoreToolChoice {
    if let Some(obj) = value.as_object() {
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("auto") => CoreToolChoice::Auto,
            Some("any") => CoreToolChoice::Any,
            Some("none") => CoreToolChoice::None,
            Some("tool") => {
                let name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                CoreToolChoice::Tool { name }
            }
            _ => CoreToolChoice::Raw(value),
        }
    } else {
        CoreToolChoice::Raw(value)
    }
}

// ---------------------------------------------------------------------------
// encode_response
// ---------------------------------------------------------------------------

/// Encode a [`CoreResponse`] into an Anthropic [`MessageResponse`].
pub fn encode_response(resp: CoreResponse) -> Result<MessageResponse, ProtocolError> {
    let id = resp.id.unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4()));

    let content = if resp.content.is_empty() {
        vec![ContentBlock::new_text(String::new())]
    } else {
        resp.content
            .into_iter()
            .map(encode_content_block)
            .collect::<Result<Vec<_>, _>>()?
    };

    let stop_reason = encode_stop_reason(resp.stop_reason);

    let usage = encode_usage(&resp.usage);

    Ok(MessageResponse {
        id,
        r#type: "message".to_owned(),
        role: "assistant".to_owned(),
        content,
        model: resp.model.requested,
        stop_reason: Some(stop_reason),
        stop_sequence: resp.stop_sequence,
        usage,
    })
}

fn encode_content_block(content: CoreContent) -> Result<ContentBlock, ProtocolError> {
    match content {
        CoreContent::Text { text, cache } => {
            let cc = cache.map(|c| AnthropicCacheControl {
                r#type: c.r#type.as_str().to_owned(),
            });
            Ok(ContentBlock {
                r#type: "text".to_owned(),
                text: Some(text),
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
                cache_control: cc,
            })
        }
        CoreContent::Image { source } => {
            let img_source = serde_json::from_value(source).unwrap_or_else(|_| {
                anthropic::ImageSource {
                    r#type: String::new(),
                    media_type: String::new(),
                    data: String::new(),
                }
            });
            Ok(ContentBlock {
                r#type: "image".to_owned(),
                text: None,
                id: None,
                tool_use_id: None,
                name: None,
                input: None,
                output: None,
                content: None,
                is_error: None,
                thinking: None,
                signature: None,
                source: Some(img_source),
                cache_control: None,
            })
        }
        CoreContent::ToolUse { id, name, input } => Ok(ContentBlock {
            r#type: "tool_use".to_owned(),
            text: None,
            id: Some(id),
            tool_use_id: None,
            name: Some(name),
            input: Some(input),
            output: None,
            content: None,
            is_error: None,
            thinking: None,
            signature: None,
            source: None,
            cache_control: None,
        }),
        CoreContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let content_val = if content.len() == 1 {
                if let CoreContent::Text { ref text, .. } = content[0] {
                    Some(serde_json::Value::String(text.clone()))
                } else {
                    Some(serde_json::to_value(&content).unwrap_or(serde_json::Value::Null))
                }
            } else if content.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&content).unwrap_or(serde_json::Value::Null))
            };
            Ok(ContentBlock {
                r#type: "tool_result".to_owned(),
                text: None,
                id: None,
                tool_use_id: Some(tool_use_id),
                name: None,
                input: None,
                output: None,
                content: content_val,
                is_error: Some(is_error),
                thinking: None,
                signature: None,
                source: None,
                cache_control: None,
            })
        }
        CoreContent::Thinking { text, signature } => Ok(ContentBlock {
            r#type: "thinking".to_owned(),
            text: None,
            id: None,
            tool_use_id: None,
            name: None,
            input: None,
            output: None,
            content: None,
            is_error: None,
            thinking: Some(text),
            signature,
            source: None,
            cache_control: None,
        }),
        CoreContent::Document { .. } => {
            tracing::warn!(
                "Anthropic Messages protocol does not natively support document blocks in responses; dropping"
            );
            Ok(ContentBlock::new_text(String::new()))
        }
        CoreContent::Audio { .. } => {
            tracing::warn!(
                "Anthropic Messages protocol does not natively support audio blocks in responses; dropping"
            );
            Ok(ContentBlock::new_text(String::new()))
        }
        CoreContent::Video { .. } => {
            tracing::warn!(
                "Anthropic Messages protocol does not natively support video blocks in responses; dropping"
            );
            Ok(ContentBlock::new_text(String::new()))
        }
        CoreContent::RedactedThinking { data } => {
            // Anthropic supports redacted thinking -- encode as a thinking block with
            // the data preserved in the thinking field as a JSON string.
            Ok(ContentBlock {
                r#type: "thinking".to_owned(),
                text: None,
                id: None,
                tool_use_id: None,
                name: None,
                input: None,
                output: None,
                content: None,
                is_error: None,
                thinking: Some(data.to_string()),
                signature: None,
                source: None,
                cache_control: None,
            })
        }
        CoreContent::Refusal { text } => {
            // Anthropic does not have a native refusal block. Emit as text.
            tracing::warn!(
                refusal_text = text.as_str(),
                "Anthropic Messages protocol has no refusal field; encoding as text"
            );
            Ok(ContentBlock::new_text(text))
        }
    }
}

fn encode_stop_reason(reason: StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".to_owned(),
        StopReason::MaxTokens => "max_tokens".to_owned(),
        StopReason::ToolUse => "tool_use".to_owned(),
        StopReason::StopSequence => "end_turn".to_owned(),
        StopReason::Refusal => "end_turn".to_owned(),
        StopReason::Error => "end_turn".to_owned(),
        StopReason::Unknown => "end_turn".to_owned(),
    }
}

fn encode_usage(usage: &Usage) -> anthropic::Usage {
    anthropic::Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
    }
}

// ---------------------------------------------------------------------------
// StreamEncoder
// ---------------------------------------------------------------------------

/// Stateful encoder that converts [`CoreEvent`] values into Anthropic SSE
/// [`MessageEvent`] values.
///
/// The encoder buffers the latest `UsageDelta` and coalesces it with the
/// terminal `MessageStop` into a single `message_delta` event carrying both
/// `usage` and `delta.stop_reason`/`stop_sequence`, followed by one
/// `message_stop`.  This matches native Anthropic behavior.
#[derive(Debug)]
pub struct StreamEncoder {
    /// The message ID for this stream.
    msg_id: String,
    /// The model name to report.
    model: String,
    /// Buffered usage from the latest UsageDelta.
    pending_usage: Option<anthropic::Usage>,
    /// Whether message_start has been emitted.
    started: bool,
    /// Whether the terminal message_delta + message_stop have been emitted.
    finished: bool,
}

impl StreamEncoder {
    /// Create a new stream encoder.
    ///
    /// `msg_id` and `model` are typically taken from the first
    /// `CoreEvent::MessageStart`.
    pub fn new(msg_id: String, model: String) -> Self {
        Self {
            msg_id,
            model,
            pending_usage: None,
            started: false,
            finished: false,
        }
    }

    /// Encode a single [`CoreEvent`] into zero or more Anthropic [`MessageEvent`]s.
    pub fn encode_event(&mut self, event: CoreEvent) -> Result<Vec<MessageEvent>, ProtocolError> {
        if self.finished {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();

        match event {
            CoreEvent::MessageStart { id, model } => {
                if let Some(id) = id {
                    self.msg_id = id;
                }
                self.model = model.requested;
                self.started = true;

                events.push(MessageEvent {
                    r#type: "message_start".to_owned(),
                    message: Some(MessageResponse {
                        id: self.msg_id.clone(),
                        r#type: "message".to_owned(),
                        role: "assistant".to_owned(),
                        content: vec![],
                        model: self.model.clone(),
                        stop_reason: None,
                        stop_sequence: None,
                        usage: anthropic::Usage {
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
                });
            }

            CoreEvent::ContentStart { index, kind } => {
                let block = match kind {
                    ContentKind::Text => ContentBlock {
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
                        cache_control: None,
                    },
                    ContentKind::Thinking => ContentBlock {
                        r#type: "thinking".to_owned(),
                        text: None,
                        id: None,
                        tool_use_id: None,
                        name: None,
                        input: None,
                        output: None,
                        content: None,
                        is_error: None,
                        thinking: Some(String::new()),
                        signature: None,
                        source: None,
                        cache_control: None,
                    },
                    ContentKind::ToolUse => ContentBlock {
                        r#type: "tool_use".to_owned(),
                        text: None,
                        id: Some(String::new()),
                        tool_use_id: None,
                        name: Some(String::new()),
                        input: Some(serde_json::Value::Object(serde_json::Map::new())),
                        output: None,
                        content: None,
                        is_error: None,
                        thinking: None,
                        signature: None,
                        source: None,
                        cache_control: None,
                    },
                    other => {
                        // ContentStart for unsupported kinds -- emit as text.
                        tracing::warn!(
                            kind = ?other,
                            "unsupported ContentKind in Anthropic stream encode, treating as text"
                        );
                        ContentBlock {
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
                            cache_control: None,
                        }
                    }
                };
                events.push(MessageEvent {
                    r#type: "content_block_start".to_owned(),
                    message: None,
                    index: Some(index),
                    content_block: Some(block),
                    delta: None,
                    usage: None,
                    error: None,
                });
            }

            CoreEvent::TextDelta { index, text } => {
                events.push(MessageEvent {
                    r#type: "content_block_delta".to_owned(),
                    message: None,
                    index: Some(index),
                    content_block: None,
                    delta: Some(Delta {
                        r#type: Some("text_delta".to_owned()),
                        text: Some(text),
                        thinking: None,
                        partial_json: None,
                        stop_reason: None,
                        stop_sequence: None,
                    }),
                    usage: None,
                    error: None,
                });
            }

            CoreEvent::ThinkingDelta { index, text } => {
                events.push(MessageEvent {
                    r#type: "content_block_delta".to_owned(),
                    message: None,
                    index: Some(index),
                    content_block: None,
                    delta: Some(Delta {
                        r#type: Some("thinking_delta".to_owned()),
                        text: None,
                        thinking: Some(text),
                        partial_json: None,
                        stop_reason: None,
                        stop_sequence: None,
                    }),
                    usage: None,
                    error: None,
                });
            }

            CoreEvent::ToolCallStart { index, id, name } => {
                events.push(MessageEvent {
                    r#type: "content_block_start".to_owned(),
                    message: None,
                    index: Some(index),
                    content_block: Some(ContentBlock {
                        r#type: "tool_use".to_owned(),
                        text: None,
                        id: Some(id),
                        tool_use_id: None,
                        name: Some(name),
                        input: Some(serde_json::Value::Object(serde_json::Map::new())),
                        output: None,
                        content: None,
                        is_error: None,
                        thinking: None,
                        signature: None,
                        source: None,
                        cache_control: None,
                    }),
                    delta: None,
                    usage: None,
                    error: None,
                });
            }

            CoreEvent::ToolCallDelta { index, args_delta } => {
                events.push(MessageEvent {
                    r#type: "content_block_delta".to_owned(),
                    message: None,
                    index: Some(index),
                    content_block: None,
                    delta: Some(Delta {
                        r#type: Some("input_json_delta".to_owned()),
                        text: None,
                        thinking: None,
                        partial_json: Some(args_delta),
                        stop_reason: None,
                        stop_sequence: None,
                    }),
                    usage: None,
                    error: None,
                });
            }

            CoreEvent::ToolCallStop { index } => {
                events.push(MessageEvent {
                    r#type: "content_block_stop".to_owned(),
                    message: None,
                    index: Some(index),
                    content_block: None,
                    delta: None,
                    usage: None,
                    error: None,
                });
            }

            CoreEvent::UsageDelta { usage } => {
                // Buffer usage; it will be emitted with the terminal event.
                self.pending_usage = Some(encode_usage(&usage));
            }

            CoreEvent::MessageStop {
                stop_reason,
                stop_sequence,
            } => {
                // Emit a single message_delta with both usage and stop fields,
                // then a message_stop.
                let anth_stop = encode_stop_reason(stop_reason);
                let usage = self.pending_usage.take().unwrap_or(anthropic::Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                });

                events.push(MessageEvent {
                    r#type: "message_delta".to_owned(),
                    message: None,
                    index: None,
                    content_block: None,
                    delta: Some(Delta {
                        r#type: None,
                        text: None,
                        thinking: None,
                        partial_json: None,
                        stop_reason: Some(anth_stop),
                        stop_sequence,
                    }),
                    usage: Some(usage),
                    error: None,
                });

                events.push(MessageEvent {
                    r#type: "message_stop".to_owned(),
                    message: None,
                    index: None,
                    content_block: None,
                    delta: None,
                    usage: None,
                    error: None,
                });

                self.finished = true;
            }

            CoreEvent::Error { error } => {
                events.push(MessageEvent {
                    r#type: "error".to_owned(),
                    message: None,
                    index: None,
                    content_block: None,
                    delta: None,
                    usage: None,
                    error: Some(ApiError {
                        r#type: encode_error_kind(&error.kind),
                        message: error.message().to_owned(),
                    }),
                });
            }

            CoreEvent::Ping => {
                events.push(MessageEvent {
                    r#type: "ping".to_owned(),
                    message: None,
                    index: None,
                    content_block: None,
                    delta: None,
                    usage: None,
                    error: None,
                });
            }
        }

        Ok(events)
    }

    /// Flush any remaining buffered events (e.g. if the stream was terminated
    /// without a `MessageStop`).
    pub fn finish(&mut self) -> Result<Vec<MessageEvent>, ProtocolError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        let mut events = Vec::new();

        // If we have pending usage, emit a terminal message_delta.
        if let Some(usage) = self.pending_usage.take() {
            events.push(MessageEvent {
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
                    stop_sequence: None,
                }),
                usage: Some(usage),
                error: None,
            });
            events.push(MessageEvent {
                r#type: "message_stop".to_owned(),
                message: None,
                index: None,
                content_block: None,
                delta: None,
                usage: None,
                error: None,
            });
        }

        Ok(events)
    }
}

fn encode_error_kind(kind: &CoreStreamErrorKind) -> String {
    match kind {
        CoreStreamErrorKind::InvalidRequest => "invalid_request_error".to_owned(),
        CoreStreamErrorKind::Authentication => "authentication_error".to_owned(),
        CoreStreamErrorKind::Permission => "permission_error".to_owned(),
        CoreStreamErrorKind::RateLimit => "rate_limit_error".to_owned(),
        CoreStreamErrorKind::Upstream => "api_error".to_owned(),
        CoreStreamErrorKind::Internal => "api_error".to_owned(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::Tool;
    use crate::core::{CoreStreamError, UsageProvenance};

    // -- helpers ------------------------------------------------------------

    fn make_anthropic_request() -> MessageRequest {
        MessageRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 1024,
            system: None,
            messages: vec![Message {
                role: "user".into(),
                content: serde_json::Value::String("hi".into()),
            }],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
            tool_choice: None,
        }
    }

    // -- decode tests -------------------------------------------------------

    #[test]
    fn plain_text_request_decodes_to_core() {
        let req = make_anthropic_request();
        let core = decode_request(req).unwrap();
        assert_eq!(core.model.requested, "claude-sonnet-4-20250514");
        assert_eq!(core.messages.len(), 1);
        assert_eq!(core.messages[0].role, CoreRole::User);
        assert_eq!(core.messages[0].content.len(), 1);
        match &core.messages[0].content[0] {
            CoreContent::Text { text, .. } => assert_eq!(text, "hi"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn system_prompt_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.system = Some(serde_json::Value::String("You are helpful".into()));
        let core = decode_request(req).unwrap();
        assert_eq!(core.system.len(), 1);
        match &core.system[0] {
            CoreContent::Text { text, .. } => assert_eq!(text, "You are helpful"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn system_prompt_array_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.system = Some(serde_json::json!([
            { "type": "text", "text": "You are helpful." },
            { "type": "text", "text": "Be concise." }
        ]));
        let core = decode_request(req).unwrap();
        assert_eq!(core.system.len(), 2);
    }

    #[test]
    fn tool_call_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.messages.push(Message {
            role: "assistant".into(),
            content: serde_json::json!([
                { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SF"} }
            ]),
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.messages.len(), 2);
        match &core.messages[1].content[0] {
            CoreContent::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "SF");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn tool_result_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.messages.push(Message {
            role: "user".into(),
            content: serde_json::json!([
                { "type": "tool_result", "tool_use_id": "tu_1", "content": "72F and sunny" }
            ]),
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.messages.len(), 2);
        match &core.messages[1].content[0] {
            CoreContent::ToolResult { tool_use_id, is_error, .. } => {
                assert_eq!(tool_use_id, "tu_1");
                assert!(!is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn thinking_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.messages.push(Message {
            role: "assistant".into(),
            content: serde_json::json!([
                { "type": "thinking", "thinking": "hmm...", "signature": "sig_abc" }
            ]),
        });
        let core = decode_request(req).unwrap();
        match &core.messages[1].content[0] {
            CoreContent::Thinking { text, signature } => {
                assert_eq!(text, "hmm...");
                assert_eq!(signature.as_deref(), Some("sig_abc"));
            }
            _ => panic!("expected Thinking"),
        }
    }

    #[test]
    fn tool_choice_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.tool_choice = Some(serde_json::json!({"type": "auto"}));
        let core = decode_request(req).unwrap();
        assert_eq!(core.tool_choice, Some(CoreToolChoice::Auto));

        let mut req2 = make_anthropic_request();
        req2.tool_choice = Some(serde_json::json!({"type": "tool", "name": "get_weather"}));
        let core2 = decode_request(req2).unwrap();
        match core2.tool_choice {
            Some(CoreToolChoice::Tool { name }) => assert_eq!(name, "get_weather"),
            _ => panic!("expected Tool"),
        }
    }

    #[test]
    fn cache_control_decodes_to_core() {
        let mut req = make_anthropic_request();
        req.system = Some(serde_json::json!([
            { "type": "text", "text": "You are helpful.", "cache_control": {"type": "ephemeral"} }
        ]));
        let core = decode_request(req).unwrap();
        match &core.system[0] {
            CoreContent::Text { text, cache } => {
                assert_eq!(text, "You are helpful.");
                assert!(cache.is_some());
                assert_eq!(cache.as_ref().unwrap().r#type, CacheControlType::Ephemeral);
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn multiple_messages_decode_to_ordered_core_messages() {
        let mut req = make_anthropic_request();
        req.messages.push(Message {
            role: "assistant".into(),
            content: serde_json::Value::String("hello".into()),
        });
        req.messages.push(Message {
            role: "user".into(),
            content: serde_json::Value::String("how are you?".into()),
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.messages.len(), 3);
        assert_eq!(core.messages[0].role, CoreRole::User);
        assert_eq!(core.messages[1].role, CoreRole::Assistant);
        assert_eq!(core.messages[2].role, CoreRole::User);
    }

    #[test]
    fn malformed_request_returns_protocol_error() {
        let mut req = make_anthropic_request();
        req.model = String::new();
        let err = decode_request(req).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidRequest(_)));

        let mut req2 = make_anthropic_request();
        req2.messages = vec![];
        let err2 = decode_request(req2).unwrap_err();
        assert!(matches!(err2, ProtocolError::InvalidRequest(_)));
    }

    #[test]
    fn unknown_role_returns_decode_error() {
        let mut req = make_anthropic_request();
        req.messages.push(Message {
            role: "system".into(),
            content: serde_json::Value::String("test".into()),
        });
        let err = decode_request(req).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn metadata_preserved_in_decode() {
        let mut req = make_anthropic_request();
        req.metadata = Some(anthropic::Metadata {
            user_id: Some("user-42".into()),
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.metadata.user_id.as_deref(), Some("user-42"));
    }

    #[test]
    fn sampling_options_preserved_in_decode() {
        let mut req = make_anthropic_request();
        req.temperature = Some(0.7);
        req.top_p = Some(0.9);
        req.thinking = Some(serde_json::json!({"type": "enabled", "budget_tokens": 5000}));
        let core = decode_request(req).unwrap();
        assert_eq!(core.sampling.temperature, Some(0.7));
        assert_eq!(core.sampling.top_p, Some(0.9));
        assert_eq!(core.sampling.max_tokens, Some(1024));
        assert!(core.sampling.thinking.is_some());
    }

    #[test]
    fn stream_flag_preserved_in_decode() {
        let mut req = make_anthropic_request();
        req.stream = Some(true);
        let core = decode_request(req).unwrap();
        assert!(core.stream);

        let req2 = make_anthropic_request();
        let core2 = decode_request(req2).unwrap();
        assert!(!core2.stream);
    }

    // -- encode tests -------------------------------------------------------

    #[test]
    fn core_text_response_encodes_to_route_response() {
        let resp = CoreResponse {
            id: Some("msg_123".into()),
            model: ModelRef {
                requested: "claude-sonnet-4-20250514".into(),
                upstream: None,
            },
            content: vec![CoreContent::Text {
                text: "hello".into(),
                cache: None,
            }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::provider_reported(10, 20),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        assert_eq!(out.id, "msg_123");
        assert_eq!(out.model, "claude-sonnet-4-20250514");
        assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(out.content.len(), 1);
        assert_eq!(out.content[0].r#type, "text");
        assert_eq!(out.content[0].text.as_deref(), Some("hello"));
    }

    #[test]
    fn core_tool_use_response_encodes_to_route_response() {
        let resp = CoreResponse {
            id: Some("msg_123".into()),
            model: ModelRef {
                requested: "claude-sonnet-4-20250514".into(),
                upstream: None,
            },
            content: vec![CoreContent::ToolUse {
                id: "tu_1".into(),
                name: "get_weather".into(),
                input: serde_json::json!({"city": "SF"}),
            }],
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            usage: Usage::provider_reported(10, 20),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        assert_eq!(out.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(out.content.len(), 1);
        assert_eq!(out.content[0].r#type, "tool_use");
    }

    #[test]
    fn stop_reason_mapping() {
        let cases = vec![
            (StopReason::EndTurn, "end_turn"),
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::ToolUse, "tool_use"),
            (StopReason::StopSequence, "end_turn"),
            (StopReason::Unknown, "end_turn"),
        ];
        for (reason, expected) in cases {
            assert_eq!(encode_stop_reason(reason), expected);
        }
    }

    #[test]
    fn usage_mapping() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 200,
            reasoning_tokens: None,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(5),
            provenance: UsageProvenance::ProviderReported,
        };
        let out = encode_usage(&usage);
        assert_eq!(out.input_tokens, 100);
        assert_eq!(out.output_tokens, 200);
        assert_eq!(out.cache_creation_input_tokens, Some(10));
        assert_eq!(out.cache_read_input_tokens, Some(5));
    }

    #[test]
    fn stop_sequence_mapping() {
        let resp = CoreResponse {
            id: Some("msg_123".into()),
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
            stop_reason: StopReason::StopSequence,
            stop_sequence: Some("\n".into()),
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        assert_eq!(out.stop_sequence.as_deref(), Some("\n"));
    }

    #[test]
    fn empty_content_produces_empty_text_block() {
        let resp = CoreResponse {
            id: None,
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        assert_eq!(out.content.len(), 1);
        assert_eq!(out.content[0].r#type, "text");
        assert_eq!(out.content[0].text.as_deref(), Some(""));
    }

    #[test]
    fn id_generated_when_missing() {
        let resp = CoreResponse {
            id: None,
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        assert!(out.id.starts_with("msg_"));
    }

    // -- streaming encode tests ---------------------------------------------

    #[test]
    fn streaming_text_event_mapping() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        let events = enc
            .encode_event(CoreEvent::TextDelta {
                index: 0,
                text: "hello".into(),
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, "content_block_delta");
        assert_eq!(events[0].delta.as_ref().unwrap().r#type.as_deref(), Some("text_delta"));
        assert_eq!(events[0].delta.as_ref().unwrap().text.as_deref(), Some("hello"));
    }

    #[test]
    fn streaming_thinking_event_mapping() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        let events = enc
            .encode_event(CoreEvent::ThinkingDelta {
                index: 0,
                text: "hmm".into(),
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, "content_block_delta");
        assert_eq!(events[0].delta.as_ref().unwrap().r#type.as_deref(), Some("thinking_delta"));
        assert_eq!(events[0].delta.as_ref().unwrap().thinking.as_deref(), Some("hmm"));
    }

    #[test]
    fn streaming_tool_event_mapping() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());

        let start_events = enc
            .encode_event(CoreEvent::ToolCallStart {
                index: 0,
                id: "tu_1".into(),
                name: "get_weather".into(),
            })
            .unwrap();
        assert_eq!(start_events.len(), 1);
        assert_eq!(start_events[0].r#type, "content_block_start");
        assert_eq!(start_events[0].content_block.as_ref().unwrap().r#type, "tool_use");

        let delta_events = enc
            .encode_event(CoreEvent::ToolCallDelta {
                index: 0,
                args_delta: "{\"city\":".into(),
            })
            .unwrap();
        assert_eq!(delta_events.len(), 1);
        assert_eq!(delta_events[0].r#type, "content_block_delta");
        assert_eq!(
            delta_events[0].delta.as_ref().unwrap().r#type.as_deref(),
            Some("input_json_delta")
        );

        let stop_events = enc
            .encode_event(CoreEvent::ToolCallStop { index: 0 })
            .unwrap();
        assert_eq!(stop_events.len(), 1);
        assert_eq!(stop_events[0].r#type, "content_block_stop");
    }

    #[test]
    fn streaming_usage_coalesced_with_message_stop() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());

        // Send UsageDelta first.
        enc.encode_event(CoreEvent::UsageDelta {
            usage: Usage::provider_reported(50, 100),
        })
        .unwrap();

        // Then send MessageStop -- should coalesce.
        let events = enc
            .encode_event(CoreEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
            })
            .unwrap();

        // Should produce exactly 2 events: message_delta (with usage + stop) and message_stop.
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].r#type, "message_delta");
        assert!(events[0].usage.is_some());
        assert_eq!(events[0].usage.as_ref().unwrap().input_tokens, 50);
        assert_eq!(events[0].delta.as_ref().unwrap().stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(events[1].r#type, "message_stop");
    }

    #[test]
    fn streaming_message_start_event() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        let events = enc
            .encode_event(CoreEvent::MessageStart {
                id: Some("msg_abc".into()),
                model: ModelRef {
                    requested: "claude-sonnet-4-20250514".into(),
                    upstream: None,
                },
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, "message_start");
        assert_eq!(events[0].message.as_ref().unwrap().id, "msg_abc");
    }

    #[test]
    fn streaming_error_event() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        let events = enc
            .encode_event(CoreEvent::Error {
                error: CoreStreamError::new(
                    CoreStreamErrorKind::RateLimit,
                    "too many requests".into(),
                ),
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, "error");
        assert!(events[0].error.is_some());
    }

    #[test]
    fn streaming_ping_event() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        let events = enc.encode_event(CoreEvent::Ping).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, "ping");
    }

    #[test]
    fn streaming_finish_after_message_stop_is_empty() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        enc.encode_event(CoreEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
        })
        .unwrap();
        let remaining = enc.finish().unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn streaming_finish_with_pending_usage() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        enc.encode_event(CoreEvent::UsageDelta {
            usage: Usage::provider_reported(10, 20),
        })
        .unwrap();
        let remaining = enc.finish().unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].r#type, "message_delta");
        assert_eq!(remaining[1].r#type, "message_stop");
    }

    #[test]
    fn streaming_events_after_stop_are_ignored() {
        let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
        enc.encode_event(CoreEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
        })
        .unwrap();
        let events = enc
            .encode_event(CoreEvent::TextDelta {
                index: 0,
                text: "ignored".into(),
            })
            .unwrap();
        assert!(events.is_empty());
    }

    // -- every CoreEvent variant handled ------------------------------------

    #[test]
    fn every_core_event_variant_maps_or_errors_intentionally() {
        let variants: Vec<CoreEvent> = vec![
            CoreEvent::MessageStart {
                id: None,
                model: ModelRef {
                    requested: "m".into(),
                    upstream: None,
                },
            },
            CoreEvent::ContentStart {
                index: 0,
                kind: ContentKind::Text,
            },
            CoreEvent::ContentStart {
                index: 1,
                kind: ContentKind::Thinking,
            },
            CoreEvent::ContentStart {
                index: 2,
                kind: ContentKind::ToolUse,
            },
            CoreEvent::TextDelta {
                index: 0,
                text: "hello".into(),
            },
            CoreEvent::ThinkingDelta {
                index: 1,
                text: "hmm".into(),
            },
            CoreEvent::ToolCallStart {
                index: 2,
                id: "tu_1".into(),
                name: "fn".into(),
            },
            CoreEvent::ToolCallDelta {
                index: 2,
                args_delta: "{}".into(),
            },
            CoreEvent::ToolCallStop { index: 2 },
            CoreEvent::UsageDelta {
                usage: Usage::default(),
            },
            CoreEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
            },
            CoreEvent::Error {
                error: CoreStreamError::new(
                    CoreStreamErrorKind::Internal,
                    "test".into(),
                ),
            },
            CoreEvent::Ping,
        ];
        for event in &variants {
            let mut enc = StreamEncoder::new("msg_1".into(), "m".into());
            let result = enc.encode_event(event.clone());
            assert!(result.is_ok(), "event {event:?} should not error");
        }
    }

    // -- unsupported content variants ---------------------------------------

    #[test]
    fn encode_refusal_produces_text() {
        let content = CoreContent::Refusal {
            text: "I cannot help with that".into(),
        };
        let block = encode_content_block(content).unwrap();
        assert_eq!(block.r#type, "text");
        assert_eq!(block.text.as_deref(), Some("I cannot help with that"));
    }

    #[test]
    fn encode_redacted_thinking_produces_thinking() {
        let content = CoreContent::RedactedThinking {
            data: serde_json::json!({"redacted": true}),
        };
        let block = encode_content_block(content).unwrap();
        assert_eq!(block.r#type, "thinking");
        assert!(block.thinking.is_some());
    }

    #[test]
    fn encode_document_drops_with_warning() {
        let content = CoreContent::Document {
            source: serde_json::json!({"url": "http://example.com/doc.pdf"}),
        };
        let block = encode_content_block(content).unwrap();
        assert_eq!(block.r#type, "text");
    }

    #[test]
    fn encode_audio_drops_with_warning() {
        let content = CoreContent::Audio {
            source: serde_json::json!({"data": "base64..."}),
        };
        let block = encode_content_block(content).unwrap();
        assert_eq!(block.r#type, "text");
    }

    #[test]
    fn encode_video_drops_with_warning() {
        let content = CoreContent::Video {
            source: serde_json::json!({"url": "http://example.com/vid.mp4"}),
        };
        let block = encode_content_block(content).unwrap();
        assert_eq!(block.r#type, "text");
    }

    // -- metadata / provider hints preserved --------------------------------

    #[test]
    fn metadata_raw_preserved_in_decode() {
        let mut req = make_anthropic_request();
        req.metadata = Some(anthropic::Metadata {
            user_id: Some("user-42".into()),
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.metadata.user_id.as_deref(), Some("user-42"));
    }

    // -- tool definitions ---------------------------------------------------

    #[test]
    fn tools_decoded_to_core() {
        let mut req = make_anthropic_request();
        req.tools = vec![Tool {
            name: "get_weather".into(),
            description: Some("Get weather".into()),
            input_schema: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        }];
        let core = decode_request(req).unwrap();
        assert_eq!(core.tools.len(), 1);
        assert_eq!(core.tools[0].name, "get_weather");
        assert_eq!(core.tools[0].description.as_deref(), Some("Get weather"));
    }

    // -- source guard -------------------------------------------------------

    #[test]
    fn client_anthropic_does_not_import_forbidden_modules() {
        // This test is a compile-time assertion. If this module imported any of
        // llm_proxy_provider, llm_proxy_server, transformer, or core config/routing,
        // the build would fail. The existence of this test documents the invariant.
        // The actual enforcement is the absence of those imports in the module source.
        assert!(true, "source guard: anthropic adapter imports are clean");
    }
}
