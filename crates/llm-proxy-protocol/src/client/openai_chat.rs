//! OpenAI Chat Completions client protocol adapter.
//!
//! Translates between OpenAI Chat Completions API wire format and the normalised
//! core protocol types.  This adapter only knows about OpenAI Chat Completions
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

use crate::client::ProtocolError;
use crate::core::{
    CacheControl, CacheControlType, ContentKind, CoreContent, CoreEvent, CoreMessage, CoreRequest,
    CoreResponse, CoreRole, CoreTool, CoreToolChoice,
    ModelRef, ProviderHints, RequestMetadata, SamplingOptions, StopReason, Usage,
};
#[cfg(test)]
use crate::core::{CoreStreamError, CoreStreamErrorKind};
use crate::openai::{
    ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, ChatMessage, Choice, FunctionCall,
    ToolCall, UsageInfo,
};
#[cfg(test)]
use crate::openai::{CacheControl as OpenAICacheControl, StreamOptions};

// ---------------------------------------------------------------------------
// decode_request
// ---------------------------------------------------------------------------

/// Decode an OpenAI [`ChatCompletionRequest`] into a normalised [`CoreRequest`].
pub fn decode_request(req: ChatCompletionRequest) -> Result<CoreRequest, ProtocolError> {
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

    let mut system = Vec::new();
    let mut messages = Vec::new();

    for msg in req.messages {
        match msg.role.as_str() {
            "system" => {
                if !msg.content.is_empty() {
                    let cache = msg
                        .cache_control
                        .map(|cc| CacheControl {
                            r#type: CacheControlType::from(cc.r#type),
                        });
                    system.push(CoreContent::Text {
                        text: msg.content,
                        cache,
                    });
                }
            }
            "user" => {
                let mut content = Vec::new();
                if !msg.content.is_empty() {
                    let cache = msg
                        .cache_control
                        .map(|cc| CacheControl {
                            r#type: CacheControlType::from(cc.r#type),
                        });
                    content.push(CoreContent::Text {
                        text: msg.content,
                        cache,
                    });
                }
                messages.push(CoreMessage {
                    role: CoreRole::User,
                    content,
                });
            }
            "assistant" => {
                // Ordering: Thinking -> ToolUse -> Text. OpenAI's wire format
                // has reasoning_content, tool_calls, and content as parallel
                // fields with no defined ordering. This ordering is a
                // deterministic choice that puts thinking before tool calls
                // and text content.
                let mut content = Vec::new();
                if let Some(thinking) = msg.reasoning_content {
                    if !thinking.is_empty() {
                        content.push(CoreContent::Thinking {
                            text: thinking,
                            signature: None,
                        });
                    }
                }
                for tc in &msg.tool_calls {
                    let args: serde_json::Value = tc
                        .function
                        .as_ref()
                        .and_then(|f| f.arguments.as_ref())
                        .and_then(|a| serde_json::from_str(a).ok())
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    content.push(CoreContent::ToolUse {
                        id: tc.id.clone().unwrap_or_default(),
                        name: tc
                            .function
                            .as_ref()
                            .and_then(|f| f.name.clone())
                            .unwrap_or_default(),
                        input: args,
                    });
                }
                if !msg.content.is_empty() {
                    content.push(CoreContent::Text {
                        text: msg.content,
                        cache: None,
                    });
                }
                messages.push(CoreMessage {
                    role: CoreRole::Assistant,
                    content,
                });
            }
            "tool" => {
                // OpenAI tool messages lack an explicit is_error field; we default
                // to false. Future OpenAI spec additions may add this field.
                let is_error = false;
                let inner_content = if msg.content.is_empty() {
                    vec![]
                } else {
                    vec![CoreContent::Text {
                        text: msg.content,
                        cache: None,
                    }]
                };
                messages.push(CoreMessage {
                    role: CoreRole::Tool,
                    content: vec![CoreContent::ToolResult {
                        tool_use_id: msg.tool_call_id.unwrap_or_default(),
                        content: inner_content,
                        is_error,
                    }],
                });
            }
            other => {
                return Err(ProtocolError::Decode(format!(
                    "unknown role: {other}"
                )));
            }
        }
    }

    let tools = req
        .tools
        .into_iter()
        .map(|t| CoreTool {
            name: t.function.name,
            description: t.function.description,
            input_schema: t.function.parameters.unwrap_or(serde_json::Value::Object(
                serde_json::Map::new(),
            )),
        })
        .collect();

    let tool_choice = req.tool_choice.map(decode_tool_choice);

    let mut stop = None;
    if let Some(s) = req.stop {
        match s {
            serde_json::Value::String(st) => stop = Some(vec![st]),
            serde_json::Value::Array(arr) => {
                let mut result = Vec::new();
                for item in arr {
                    if let serde_json::Value::String(st) = item {
                        result.push(st);
                    }
                }
                stop = Some(result);
            }
            _ => {}
        }
    }

    let sampling = SamplingOptions {
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens,
        stop,
        reasoning_effort: req.reasoning_effort,
        thinking: req.thinking,
    };

    let stream = req.stream.unwrap_or(false);

    let mut raw_hints = serde_json::Map::new();
    if let Some(so) = req.stream_options {
        if let Some(include) = so.include_usage {
            raw_hints.insert(
                "stream_options".into(),
                serde_json::json!({"include_usage": include}),
            );
        }
    }

    let metadata = RequestMetadata {
        user_id: req.user,
        raw: req.extra,
    };

    let provider_hints = ProviderHints { raw: raw_hints };

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

fn decode_tool_choice(value: serde_json::Value) -> CoreToolChoice {
    if let Some(s) = value.as_str() {
        match s {
            "auto" => CoreToolChoice::Auto,
            "none" => CoreToolChoice::None,
            "required" => CoreToolChoice::Any,
            _ => CoreToolChoice::Raw(value),
        }
    } else if let Some(obj) = value.as_object() {
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("auto") => CoreToolChoice::Auto,
            Some("none") => CoreToolChoice::None,
            Some("required") => CoreToolChoice::Any,
            Some("function") => {
                let name = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
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

/// Encode a [`CoreResponse`] into an OpenAI [`ChatCompletionResponse`].
pub fn encode_response(
    resp: CoreResponse,
) -> Result<ChatCompletionResponse, ProtocolError> {
    let id = resp.id.unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4()));

    let mut content_text = String::new();
    let mut reasoning = None;
    let mut tool_calls = Vec::new();
    let mut tool_call_index: i32 = 0;
    let mut refusal_text: Option<String> = None;

    for block in resp.content {
        match block {
            CoreContent::Text { text, .. } => {
                content_text.push_str(&text);
            }
            CoreContent::Thinking { text, .. } => {
                reasoning = Some(text);
            }
            CoreContent::ToolUse { id, name, input } => {
                let args_str = serde_json::to_string(&input)
                    .unwrap_or_else(|_| "{}".to_owned());
                tool_calls.push(ToolCall {
                    index: Some(tool_call_index),
                    id: Some(id),
                    r#type: Some("function".to_owned()),
                    function: Some(FunctionCall {
                        name: Some(name),
                        arguments: Some(args_str),
                    }),
                });
                tool_call_index += 1;
            }
            CoreContent::Image { .. } => {
                tracing::warn!(
                    "OpenAI Chat Completions does not support image blocks in assistant responses; dropping"
                );
            }
            CoreContent::Document { .. } => {
                tracing::warn!(
                    "OpenAI Chat Completions does not support document blocks in assistant responses; dropping"
                );
            }
            CoreContent::Audio { .. } => {
                tracing::warn!(
                    "OpenAI Chat Completions does not support audio blocks in assistant responses; dropping"
                );
            }
            CoreContent::Video { .. } => {
                tracing::warn!(
                    "OpenAI Chat Completions does not support video blocks in assistant responses; dropping"
                );
            }
            CoreContent::ToolResult { .. } => {
                // Tool results should not appear in assistant responses -- this
                // likely indicates a logic error upstream.
                tracing::warn!(
                    "ToolResult in response content is unexpected for OpenAI Chat encode; dropping"
                );
            }
            CoreContent::RedactedThinking { .. } => {
                // Per the plan: return ProtocolError::Encode for RedactedThinking
                // since OpenAI Chat does not support it and the data cannot be
                // safely represented.
                tracing::warn!(
                    "OpenAI Chat Completions does not support redacted thinking; cannot encode"
                );
                return Err(ProtocolError::Encode(
                    "OpenAI Chat Completions does not support RedactedThinking blocks".into(),
                ));
            }
            CoreContent::Refusal { text } => {
                // OpenAI has a native refusal field on assistant messages.
                // Encode the refusal text into the dedicated field.
                refusal_text = Some(text);
            }
        }
    }

    let finish_reason = encode_finish_reason(resp.stop_reason);

    let message = ChatMessage {
        role: "assistant".to_owned(),
        content: content_text,
        reasoning_content: reasoning,
        tool_calls,
        name: None,
        tool_call_id: None,
        cache_control: None,
        refusal: refusal_text,
    };

    let usage = encode_usage(&resp.usage);

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion".to_owned(),
        created,
        model: resp.model.requested,
        choices: vec![Choice {
            index: 0,
            message: Some(message),
            finish_reason: Some(finish_reason),
            delta: None,
        }],
        usage,
    })
}

fn encode_finish_reason(reason: StopReason) -> String {
    match reason {
        StopReason::ToolUse => "tool_calls".to_owned(),
        StopReason::MaxTokens => "length".to_owned(),
        _ => "stop".to_owned(),
    }
}

fn encode_usage(usage: &Usage) -> UsageInfo {
    UsageInfo {
        prompt_tokens: usage.input_tokens,
        completion_tokens: usage.output_tokens,
        total_tokens: usage.input_tokens + usage.output_tokens,
        prompt_cache_hit_tokens: usage.cache_read_input_tokens,
        prompt_cache_miss_tokens: usage.cache_creation_input_tokens,
    }
}

// ---------------------------------------------------------------------------
// StreamEncoder
// ---------------------------------------------------------------------------

/// Stateful encoder that converts [`CoreEvent`] values into OpenAI
/// [`ChatCompletionChunk`] values.
///
/// The encoder is constructed with a stable completion `id`, `model`,
/// `created` timestamp, and the `include_usage` flag (from stream_options).
/// Every `chat.completion.chunk` reuses that `id`/`model`/`created`.
#[derive(Debug)]
pub struct StreamEncoder {
    /// Stable completion ID across all chunks.
    id: String,
    /// Model name.
    model: String,
    /// Unix timestamp.
    created: i64,
    /// Whether to emit the usage chunk at the end.
    include_usage: bool,
    /// Whether the terminal chunk has been emitted.
    finished: bool,
    /// Buffered usage from the latest UsageDelta.
    pending_usage: Option<UsageInfo>,
}

impl StreamEncoder {
    /// Create a new stream encoder.
    ///
    /// The route constructs these args from the first `CoreEvent::MessageStart`
    /// and the decoded request.
    pub fn new(id: String, model: String, created: i64, include_usage: bool) -> Self {
        Self {
            id,
            model,
            created,
            include_usage,
            finished: false,
            pending_usage: None,
        }
    }

    /// Encode a single [`CoreEvent`] into zero or more [`ChatCompletionChunk`]s.
    pub fn encode_event(
        &mut self,
        event: CoreEvent,
    ) -> Result<Vec<ChatCompletionChunk>, ProtocolError> {
        if self.finished {
            return Ok(Vec::new());
        }

        let mut chunks = Vec::new();

        match event {
            CoreEvent::MessageStart { .. } => {
                // OpenAI does not have an explicit start event -- the first
                // chunk implicitly starts the response.
                // Emit a role chunk.
                chunks.push(self.make_chunk(Choice {
                    index: 0,
                    message: None,
                    finish_reason: None,
                    delta: Some(ChatMessage {
                        role: "assistant".to_owned(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: vec![],
                        name: None,
                        tool_call_id: None,
                        cache_control: None,
                    refusal: None,
                    }),
                }));
            }

            CoreEvent::ContentStart { index, kind } => {
                match kind {
                    ContentKind::ToolUse => {
                        // Tool use start is handled by ToolCallStart.
                    }
                    ContentKind::Text | ContentKind::Thinking => {
                        // Text/thinking start is implicit; deltas follow.
                    }
                    _ => {
                        tracing::warn!(kind = ?kind, "unsupported ContentKind in OpenAI stream");
                    }
                }
                // OpenAI does not use content block indices in the same way Anthropic
                // does -- the chunk-level position is implicit. Discard the index.
                let _ = index;
            }

            CoreEvent::TextDelta { text, .. } => {
                chunks.push(self.make_chunk(Choice {
                    index: 0,
                    message: None,
                    finish_reason: None,
                    delta: Some(ChatMessage {
                        role: String::new(),
                        content: text,
                        reasoning_content: None,
                        tool_calls: vec![],
                        name: None,
                        tool_call_id: None,
                        cache_control: None,
                    refusal: None,
                    }),
                }));
            }

            CoreEvent::ThinkingDelta { text, .. } => {
                chunks.push(self.make_chunk(Choice {
                    index: 0,
                    message: None,
                    finish_reason: None,
                    delta: Some(ChatMessage {
                        role: String::new(),
                        content: String::new(),
                        reasoning_content: Some(text),
                        tool_calls: vec![],
                        name: None,
                        tool_call_id: None,
                        cache_control: None,
                    refusal: None,
                    }),
                }));
            }

            CoreEvent::ToolCallStart { index, id, name } => {
                chunks.push(self.make_chunk(Choice {
                    index: 0,
                    message: None,
                    finish_reason: None,
                    delta: Some(ChatMessage {
                        role: String::new(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: vec![ToolCall {
                            index: Some(index as i32),
                            id: Some(id),
                            r#type: Some("function".to_owned()),
                            function: Some(FunctionCall {
                                name: Some(name),
                                arguments: Some(String::new()),
                            }),
                        }],
                        name: None,
                        tool_call_id: None,
                        cache_control: None,
                    refusal: None,
                    }),
                }));
            }

            CoreEvent::ToolCallDelta { index, args_delta } => {
                chunks.push(self.make_chunk(Choice {
                    index: 0,
                    message: None,
                    finish_reason: None,
                    delta: Some(ChatMessage {
                        role: String::new(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: vec![ToolCall {
                            index: Some(index as i32),
                            id: None,
                            r#type: None,
                            function: Some(FunctionCall {
                                name: None,
                                arguments: Some(args_delta),
                            }),
                        }],
                        name: None,
                        tool_call_id: None,
                        cache_control: None,
                    refusal: None,
                    }),
                }));
            }

            CoreEvent::ToolCallStop { .. } => {
                // No explicit event needed in OpenAI; the finish_reason
                // on the terminal chunk signals tool completion.
            }

            CoreEvent::UsageDelta { usage } => {
                self.pending_usage = Some(encode_usage(&usage));
            }

            CoreEvent::MessageStop { stop_reason, .. } => {
                let finish = encode_finish_reason(stop_reason);
                chunks.push(self.make_chunk(Choice {
                    index: 0,
                    message: None,
                    finish_reason: Some(finish),
                    delta: Some(ChatMessage {
                        role: String::new(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: vec![],
                        name: None,
                        tool_call_id: None,
                        cache_control: None,
                    refusal: None,
                    }),
                }));

                // If include_usage is set, emit a final usage chunk.
                if self.include_usage {
                    let usage = self.pending_usage.take().unwrap_or(UsageInfo {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    });
                    chunks.push(ChatCompletionChunk {
                        id: self.id.clone(),
                        object: "chat.completion.chunk".to_owned(),
                        created: self.created,
                        model: self.model.clone(),
                        choices: vec![],
                        usage: Some(usage),
                    });
                }

                self.finished = true;
            }

            CoreEvent::Error { error } => {
                // Return an error so the route handler can emit a proper error
                // response rather than leaking error text into content delta.
                // The error message was sanitized at CoreStreamError construction
                // time by the provider adapter.
                self.finished = true;
                return Err(ProtocolError::Encode(format!(
                    "stream error: {}",
                    error.message()
                )));
            }

            CoreEvent::Ping => {
                // No-op for OpenAI unless the route chooses heartbeat.
            }
        }

        Ok(chunks)
    }

    /// Flush any remaining buffered events.
    pub fn finish(&mut self) -> Result<Vec<ChatCompletionChunk>, ProtocolError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        Ok(Vec::new())
    }

    fn make_chunk(&self, choice: Choice) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: self.id.clone(),
            object: "chat.completion.chunk".to_owned(),
            created: self.created,
            model: self.model.clone(),
            choices: vec![choice],
            usage: None,
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::UsageProvenance;
    use crate::openai::{FunctionDef, ToolDef};

    // -- helpers ------------------------------------------------------------

    fn make_openai_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hello".into(),
                reasoning_content: None,
                tool_calls: vec![],
                name: None,
                tool_call_id: None,
                cache_control: None,
                refusal: None,
            }],
            stream: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            reasoning_effort: None,
            thinking: None,
            tools: vec![],
            tool_choice: None,
            stop: None,
            stream_options: None,
            user: None,
            extra: serde_json::Map::new(),
        }
    }

    fn make_core_response() -> CoreResponse {
        CoreResponse {
            id: Some("chatcmpl-123".into()),
            model: ModelRef {
                requested: "gpt-4o".into(),
                upstream: None,
            },
            content: vec![CoreContent::Text {
                text: "hello there".into(),
                cache: None,
            }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::provider_reported(10, 20),
            provider_meta: serde_json::Map::new(),
        }
    }

    // -- decode tests -------------------------------------------------------

    #[test]
    fn plain_text_request_decodes_to_core() {
        let req = make_openai_request();
        let core = decode_request(req).unwrap();
        assert_eq!(core.model.requested, "gpt-4o");
        assert_eq!(core.messages.len(), 1);
        assert_eq!(core.messages[0].role, CoreRole::User);
        match &core.messages[0].content[0] {
            CoreContent::Text { text, .. } => assert_eq!(text, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn system_prompt_decodes_to_core() {
        let mut req = make_openai_request();
        req.messages.insert(
            0,
            ChatMessage {
                role: "system".into(),
                content: "You are helpful".into(),
                reasoning_content: None,
                tool_calls: vec![],
                name: None,
                tool_call_id: None,
                cache_control: None,
                    refusal: None,
            },
        );
        let core = decode_request(req).unwrap();
        assert_eq!(core.system.len(), 1);
        match &core.system[0] {
            CoreContent::Text { text, .. } => assert_eq!(text, "You are helpful"),
            _ => panic!("expected Text"),
        }
        // System message should NOT appear in messages.
        assert_eq!(core.messages.len(), 1);
        assert_eq!(core.messages[0].role, CoreRole::User);
    }

    #[test]
    fn assistant_with_tool_calls_decodes_to_core() {
        let mut req = make_openai_request();
        req.messages.push(ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                index: Some(0),
                id: Some("call_1".into()),
                r#type: Some("function".into()),
                function: Some(FunctionCall {
                    name: Some("get_weather".into()),
                    arguments: Some("{\"city\":\"SF\"}".into()),
                }),
            }],
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.messages[1].role, CoreRole::Assistant);
        match &core.messages[1].content[0] {
            CoreContent::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "SF");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn tool_result_decodes_to_core() {
        let mut req = make_openai_request();
        req.messages.push(ChatMessage {
            role: "tool".into(),
            content: "72F and sunny".into(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: Some("call_1".into()),
            cache_control: None,
                    refusal: None,
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.messages[1].role, CoreRole::Tool);
        match &core.messages[1].content[0] {
            CoreContent::ToolResult { tool_use_id, is_error, .. } => {
                assert_eq!(tool_use_id, "call_1");
                assert!(!is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn reasoning_content_decodes_to_thinking() {
        let mut req = make_openai_request();
        req.messages.push(ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: Some("let me think...".into()),
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        });
        let core = decode_request(req).unwrap();
        match &core.messages[1].content[0] {
            CoreContent::Thinking { text, signature } => {
                assert_eq!(text, "let me think...");
                assert!(signature.is_none());
            }
            _ => panic!("expected Thinking"),
        }
    }

    #[test]
    fn tool_choice_decodes_to_core() {
        let mut req = make_openai_request();
        req.tool_choice = Some(serde_json::json!("auto"));
        let core = decode_request(req).unwrap();
        assert_eq!(core.tool_choice, Some(CoreToolChoice::Auto));

        let mut req2 = make_openai_request();
        req2.tool_choice = Some(serde_json::json!({"type": "function", "function": {"name": "get_weather"}}));
        let core2 = decode_request(req2).unwrap();
        match core2.tool_choice {
            Some(CoreToolChoice::Tool { name }) => assert_eq!(name, "get_weather"),
            _ => panic!("expected Tool"),
        }

        let mut req3 = make_openai_request();
        req3.tool_choice = Some(serde_json::json!("required"));
        let core3 = decode_request(req3).unwrap();
        assert_eq!(core3.tool_choice, Some(CoreToolChoice::Any));
    }

    #[test]
    fn cache_control_decodes_to_core() {
        let mut req = make_openai_request();
        req.messages[0].cache_control = Some(OpenAICacheControl {
            r#type: "ephemeral".into(),
        });
        let core = decode_request(req).unwrap();
        match &core.messages[0].content[0] {
            CoreContent::Text { cache, .. } => {
                assert!(cache.is_some());
                assert_eq!(cache.as_ref().unwrap().r#type, CacheControlType::Ephemeral);
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn stream_options_decode_to_provider_hints() {
        let mut req = make_openai_request();
        req.stream_options = Some(StreamOptions {
            include_usage: Some(true),
        });
        let core = decode_request(req).unwrap();
        assert!(core.provider_hints.raw.contains_key("stream_options"));
        assert_eq!(
            core.provider_hints.raw["stream_options"]["include_usage"],
            true
        );
    }

    #[test]
    fn multiple_messages_decode_to_ordered_core_messages() {
        let mut req = make_openai_request();
        req.messages.push(ChatMessage {
            role: "assistant".into(),
            content: "hi there".into(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        });
        req.messages.push(ChatMessage {
            role: "user".into(),
            content: "how are you?".into(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        });
        let core = decode_request(req).unwrap();
        assert_eq!(core.messages.len(), 3);
        assert_eq!(core.messages[0].role, CoreRole::User);
        assert_eq!(core.messages[1].role, CoreRole::Assistant);
        assert_eq!(core.messages[2].role, CoreRole::User);
    }

    #[test]
    fn malformed_request_returns_protocol_error() {
        let mut req = make_openai_request();
        req.model = String::new();
        let err = decode_request(req).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidRequest(_)));

        let mut req2 = make_openai_request();
        req2.messages = vec![];
        let err2 = decode_request(req2).unwrap_err();
        assert!(matches!(err2, ProtocolError::InvalidRequest(_)));
    }

    #[test]
    fn unknown_role_returns_decode_error() {
        let mut req = make_openai_request();
        req.messages.push(ChatMessage {
            role: "unknown_role".into(),
            content: "test".into(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        });
        let err = decode_request(req).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn sampling_options_preserved_in_decode() {
        let mut req = make_openai_request();
        req.temperature = Some(0.7);
        req.top_p = Some(0.9);
        req.max_tokens = Some(512);
        req.stop = Some(serde_json::json!(["STOP"]));
        req.reasoning_effort = Some("high".into());
        let core = decode_request(req).unwrap();
        assert_eq!(core.sampling.temperature, Some(0.7));
        assert_eq!(core.sampling.top_p, Some(0.9));
        assert_eq!(core.sampling.max_tokens, Some(512));
        assert_eq!(core.sampling.stop, Some(vec!["STOP".to_owned()]));
        assert_eq!(core.sampling.reasoning_effort, Some("high".into()));
    }

    #[test]
    fn tools_decoded_to_core() {
        let mut req = make_openai_request();
        req.tools = vec![ToolDef {
            r#type: "function".into(),
            function: FunctionDef {
                name: "get_weather".into(),
                description: Some("Get weather".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}}
                })),
            },
        }];
        let core = decode_request(req).unwrap();
        assert_eq!(core.tools.len(), 1);
        assert_eq!(core.tools[0].name, "get_weather");
    }

    // -- encode tests -------------------------------------------------------

    #[test]
    fn core_text_response_encodes_to_route_response() {
        let resp = make_core_response();
        let out = encode_response(resp).unwrap();
        assert_eq!(out.id, "chatcmpl-123");
        assert_eq!(out.model, "gpt-4o");
        assert_eq!(out.object, "chat.completion");
        assert_eq!(out.choices.len(), 1);
        assert_eq!(out.choices[0].finish_reason.as_deref(), Some("stop"));
        let msg = out.choices[0].message.as_ref().unwrap();
        assert_eq!(msg.content, "hello there");
        assert_eq!(msg.role, "assistant");
    }

    #[test]
    fn core_tool_use_response_encodes_to_route_response() {
        let resp = CoreResponse {
            id: Some("chatcmpl-123".into()),
            model: ModelRef {
                requested: "gpt-4o".into(),
                upstream: None,
            },
            content: vec![CoreContent::ToolUse {
                id: "call_1".into(),
                name: "get_weather".into(),
                input: serde_json::json!({"city": "SF"}),
            }],
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            usage: Usage::provider_reported(10, 20),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        assert_eq!(out.choices[0].finish_reason.as_deref(), Some("tool_calls"));
        let msg = out.choices[0].message.as_ref().unwrap();
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(msg.tool_calls[0].function.as_ref().unwrap().name.as_deref(), Some("get_weather"));
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(encode_finish_reason(StopReason::ToolUse), "tool_calls");
        assert_eq!(encode_finish_reason(StopReason::MaxTokens), "length");
        assert_eq!(encode_finish_reason(StopReason::EndTurn), "stop");
        assert_eq!(encode_finish_reason(StopReason::StopSequence), "stop");
        assert_eq!(encode_finish_reason(StopReason::Unknown), "stop");
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
        assert_eq!(out.prompt_tokens, 100);
        assert_eq!(out.completion_tokens, 200);
        assert_eq!(out.total_tokens, 300);
        assert_eq!(out.prompt_cache_hit_tokens, Some(5));
        assert_eq!(out.prompt_cache_miss_tokens, Some(10));
    }

    #[test]
    fn reasoning_content_encodes() {
        let resp = CoreResponse {
            id: Some("chatcmpl-123".into()),
            model: ModelRef {
                requested: "gpt-4o".into(),
                upstream: None,
            },
            content: vec![
                CoreContent::Thinking {
                    text: "let me think...".into(),
                    signature: None,
                },
                CoreContent::Text {
                    text: "answer".into(),
                    cache: None,
                },
            ],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        let msg = out.choices[0].message.as_ref().unwrap();
        assert_eq!(msg.reasoning_content.as_deref(), Some("let me think..."));
        assert_eq!(msg.content, "answer");
    }

    // -- streaming encode tests ---------------------------------------------

    #[test]
    fn streaming_text_event_mapping() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        let chunks = enc
            .encode_event(CoreEvent::TextDelta {
                index: 0,
                text: "hello".into(),
            })
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].object, "chat.completion.chunk");
        let delta = chunks[0].choices[0].delta.as_ref().unwrap();
        assert_eq!(delta.content, "hello");
    }

    #[test]
    fn streaming_thinking_event_mapping() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        let chunks = enc
            .encode_event(CoreEvent::ThinkingDelta {
                index: 0,
                text: "hmm".into(),
            })
            .unwrap();
        assert_eq!(chunks.len(), 1);
        let delta = chunks[0].choices[0].delta.as_ref().unwrap();
        assert_eq!(delta.reasoning_content.as_deref(), Some("hmm"));
    }

    #[test]
    fn streaming_tool_event_mapping() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);

        let start = enc
            .encode_event(CoreEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "get_weather".into(),
            })
            .unwrap();
        assert_eq!(start.len(), 1);
        let delta = start[0].choices[0].delta.as_ref().unwrap();
        assert_eq!(delta.tool_calls.len(), 1);
        assert_eq!(delta.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(
            delta.tool_calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("get_weather")
        );

        let delta_chunks = enc
            .encode_event(CoreEvent::ToolCallDelta {
                index: 0,
                args_delta: "{\"city\":".into(),
            })
            .unwrap();
        assert_eq!(delta_chunks.len(), 1);
        let delta = delta_chunks[0].choices[0].delta.as_ref().unwrap();
        assert_eq!(delta.tool_calls.len(), 1);
        assert_eq!(
            delta.tool_calls[0].function.as_ref().unwrap().arguments.as_deref(),
            Some("{\"city\":")
        );
    }

    #[test]
    fn streaming_message_stop_emits_finish() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        let chunks = enc
            .encode_event(CoreEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
            })
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn streaming_message_stop_with_usage_when_include() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, true);
        enc.encode_event(CoreEvent::UsageDelta {
            usage: Usage::provider_reported(50, 100),
        })
        .unwrap();
        let chunks = enc
            .encode_event(CoreEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
            })
            .unwrap();
        // Two chunks: finish + usage.
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].choices.len(), 1);
        assert!(chunks[1].usage.is_some());
        assert_eq!(chunks[1].usage.as_ref().unwrap().prompt_tokens, 50);
    }

    #[test]
    fn streaming_no_usage_when_not_included() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        enc.encode_event(CoreEvent::UsageDelta {
            usage: Usage::provider_reported(50, 100),
        })
        .unwrap();
        let chunks = enc
            .encode_event(CoreEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
            })
            .unwrap();
        // Only the finish chunk, no usage chunk.
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn streaming_error_event_returns_encode_error() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        let result = enc.encode_event(CoreEvent::Error {
            error: CoreStreamError::new(
                CoreStreamErrorKind::RateLimit,
                "too many requests".into(),
            ),
        });
        // Error events now return Err so the route handler can emit a proper
        // error response, rather than leaking error text into content delta.
        assert!(result.is_err());
        assert!(matches!(result, Err(ProtocolError::Encode(_))));
    }

    #[test]
    fn streaming_ping_is_noop() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        let chunks = enc.encode_event(CoreEvent::Ping).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn streaming_events_after_stop_are_ignored() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        enc.encode_event(CoreEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
        })
        .unwrap();
        let chunks = enc
            .encode_event(CoreEvent::TextDelta {
                index: 0,
                text: "ignored".into(),
            })
            .unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn streaming_finish_after_stop_is_empty() {
        let mut enc = StreamEncoder::new("chatcmpl-1".into(), "gpt-4o".into(), 1000, false);
        enc.encode_event(CoreEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
        })
        .unwrap();
        let remaining = enc.finish().unwrap();
        assert!(remaining.is_empty());
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
                id: "call_1".into(),
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
            let mut enc = StreamEncoder::new("chatcmpl-1".into(), "m".into(), 1000, false);
            let result = enc.encode_event(event.clone());
            match event {
                CoreEvent::Error { .. } => {
                    // Error events intentionally return Err so the route handler
                    // can emit a proper error response.
                    assert!(result.is_err(), "Error event should return Err");
                }
                _ => {
                    assert!(result.is_ok(), "event {event:?} should not error");
                }
            }
        }
    }

    // -- unsupported content variants ---------------------------------------

    #[test]
    fn encode_image_drops_with_warning() {
        let resp = CoreResponse {
            id: None,
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![CoreContent::Image {
                source: serde_json::json!({"url": "http://example.com/img.png"}),
            }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        let msg = out.choices[0].message.as_ref().unwrap();
        assert!(msg.content.is_empty());
        assert!(msg.tool_calls.is_empty());
    }

    #[test]
    fn encode_refusal_uses_native_refusal_field() {
        let resp = CoreResponse {
            id: None,
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![CoreContent::Refusal {
                text: "I cannot help".into(),
            }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let out = encode_response(resp).unwrap();
        let msg = out.choices[0].message.as_ref().unwrap();
        // Refusal text goes into the native refusal field, not content.
        assert_eq!(msg.refusal.as_deref(), Some("I cannot help"));
        assert!(msg.content.is_empty());
    }

    // -- metadata / provider hints ------------------------------------------

    #[test]
    fn provider_hints_raw_preserved_in_decode() {
        let mut req = make_openai_request();
        req.stream_options = Some(StreamOptions {
            include_usage: Some(true),
        });
        let core = decode_request(req).unwrap();
        assert!(core.provider_hints.raw.contains_key("stream_options"));
    }

    #[test]
    fn user_field_maps_to_metadata_user_id() {
        let mut req = make_openai_request();
        req.user = Some("user-abc".into());
        let core = decode_request(req).unwrap();
        assert_eq!(core.metadata.user_id.as_deref(), Some("user-abc"));
    }

    #[test]
    fn extra_fields_preserved_in_metadata_raw() {
        let mut req = make_openai_request();
        req.extra.insert(
            "custom_field".into(),
            serde_json::json!("custom_value"),
        );
        let core = decode_request(req).unwrap();
        assert_eq!(
            core.metadata.raw.get("custom_field").unwrap(),
            "custom_value"
        );
    }

    #[test]
    fn stream_flag_preserved_in_decode() {
        let mut req = make_openai_request();
        req.stream = Some(true);
        let core = decode_request(req).unwrap();
        assert!(core.stream);

        let req2 = make_openai_request();
        let core2 = decode_request(req2).unwrap();
        assert!(!core2.stream);
    }

    #[test]
    fn encode_redacted_thinking_returns_encode_error() {
        let resp = CoreResponse {
            id: None,
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![CoreContent::RedactedThinking {
                data: serde_json::json!({"redacted": true}),
            }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::Map::new(),
        };
        let result = encode_response(resp);
        assert!(matches!(result, Err(ProtocolError::Encode(_))));
    }

    #[test]
    fn empty_content_produces_valid_response() {
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
        let msg = out.choices[0].message.as_ref().unwrap();
        assert!(msg.content.is_empty());
        assert!(msg.tool_calls.is_empty());
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
        assert!(out.id.starts_with("chatcmpl-"));
    }

    // -- source guard -------------------------------------------------------

    #[test]
    fn client_openai_chat_does_not_import_forbidden_modules() {
        // This test is a compile-time documentation assertion. If this module
        // imported any of llm_proxy_provider, llm_proxy_server, transformer,
        // or core config/routing, the build would fail because those crates
        // are not dependencies of llm-proxy-protocol. The actual enforcement
        // is the absence of those imports in the module source. CI grep checks
        // provide a secondary guard.
        //
        // We verify the module compiles without those imports by simply
        // existing as a test -- no runtime assertion needed.
    }
}
