//! OpenAI Responses API provider adapter.
//!
//! Translates between core types and the Responses API wire format:
//!
//! ```text
//! CoreRequest  -> ResponsesRequest
//! ResponsesResponse -> CoreResponse
//! ResponsesChunk stream -> CoreEvent stream
//! ```

use llm_proxy_protocol::core::{
    ContentKind, CoreContent, CoreEvent, CoreRequest, CoreResponse, CoreRole,
    ModelRef, StopReason, Usage, UsageProvenance,
};
use llm_proxy_protocol::zen::{
    ResponsesChunk, ResponsesInput, ResponsesReasoning,
    ResponsesRequest, ResponsesResponse, ResponsesTool, ResponsesUsage,
};

use super::{build_proxy_request, expand_url_template, response_model_ref, ProviderAdapterTarget, ProviderStreamDecoder};
use crate::error::ProviderError;
use crate::sse::SseFrame;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Adapter for the OpenAI Responses API.
#[derive(Debug, Clone)]
pub struct ResponsesAdapter;

// ---------------------------------------------------------------------------
// Stream decoder
// ---------------------------------------------------------------------------

/// Stateful stream decoder for Responses API SSE frames.
#[derive(Debug)]
pub struct ResponsesStreamDecoder {
    model_ref: ModelRef,
    started: bool,
    content_index: usize,
    content_started: bool,
    stop_sent: bool,
}

impl ProviderStreamDecoder for ResponsesStreamDecoder {
    fn decode_frame(&mut self, frame: &SseFrame) -> Result<Vec<CoreEvent>, ProviderError> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(vec![]);
        }

        let chunk: ResponsesChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(data, "malformed Responses chunk, skipping");
                return Ok(vec![]);
            }
        };

        let mut events = Vec::new();

        match chunk.r#type.as_str() {
            "response.created" | "response.in_progress" => {
                if !self.started {
                    self.started = true;
                    events.push(CoreEvent::MessageStart {
                        id: chunk.id.clone(),
                        model: self.model_ref.clone(),
                    });
                }
            }
            "response.output_item.added" => {
                if let Some(ref outputs) = chunk.output {
                    for output in outputs {
                        if output.r#type == "message" {
                            if let Some(ref content_blocks) = output.content {
                                for content in content_blocks {
                                    if content.r#type == "output_text" {
                                        if !self.content_started {
                                            self.content_started = true;
                                            events.push(CoreEvent::ContentStart {
                                                index: self.content_index,
                                                kind: ContentKind::Text,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "response.output_text.delta" => {
                if let Some(ref delta) = chunk.delta {
                    if !delta.is_empty() {
                        if !self.content_started {
                            self.content_started = true;
                            events.push(CoreEvent::ContentStart {
                                index: self.content_index,
                                kind: ContentKind::Text,
                            });
                        }
                        events.push(CoreEvent::TextDelta {
                            index: self.content_index,
                            text: delta.clone(),
                        });
                    }
                }
            }
            "response.output_text.done" => {
                if self.content_started {
                    self.content_started = false;
                    self.content_index += 1;
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(ref delta) = chunk.delta {
                    if !delta.is_empty() {
                        events.push(CoreEvent::ToolCallDelta {
                            index: self.content_index,
                            args_delta: delta.clone(),
                        });
                    }
                }
            }
            "response.function_call_arguments.done" => {
                // Tool call complete.
                events.push(CoreEvent::ToolCallStop {
                    index: self.content_index,
                });
                self.content_index += 1;
            }
            "response.completed" => {
                self.close_content_if_open(&mut events);

                // Extract usage.
                if let Some(ref usage) = chunk.usage {
                    events.push(CoreEvent::UsageDelta {
                        usage: build_responses_usage(usage),
                    });
                }

                // Extract stop reason from output if present.
                let stop_reason = if let Some(ref outputs) = chunk.output {
                    outputs
                        .iter()
                        .find(|o| o.r#type == "function_call")
                        .map(|_| StopReason::ToolUse)
                        .unwrap_or(StopReason::EndTurn)
                } else {
                    StopReason::EndTurn
                };

                if !self.stop_sent {
                    self.stop_sent = true;
                    events.push(CoreEvent::MessageStop {
                        stop_reason,
                        stop_sequence: None,
                    });
                }
            }
            "response.done" => {
                self.close_content_if_open(&mut events);

                if let Some(ref usage) = chunk.usage {
                    events.push(CoreEvent::UsageDelta {
                        usage: build_responses_usage(usage),
                    });
                }

                if !self.stop_sent {
                    self.stop_sent = true;
                    events.push(CoreEvent::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        stop_sequence: None,
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

        self.close_content_if_open(&mut events);

        if !self.stop_sent {
            self.stop_sent = true;
            events.push(CoreEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
            });
        }

        Ok(events)
    }
}

impl ResponsesStreamDecoder {
    fn close_content_if_open(&mut self, _events: &mut Vec<CoreEvent>) {
        if self.content_started {
            self.content_started = false;
            self.content_index += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// ResponsesAdapter impl
// ---------------------------------------------------------------------------

impl ResponsesAdapter {
    /// Encode a core request into a Responses API request.
    pub fn encode_request(
        &self,
        core: &CoreRequest,
        target: &ProviderAdapterTarget,
    ) -> Result<super::ProxyRequest, ProviderError> {
        let mut input = Vec::new();

        // System prompt as first developer message.
        if !core.system.is_empty() {
            let system_text: String = core
                .system
                .iter()
                .filter_map(|c| match c {
                    CoreContent::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !system_text.is_empty() {
                input.push(ResponsesInput {
                    role: "developer".to_owned(),
                    content: Some(serde_json::Value::String(system_text)),
                });
            }
        }

        // Conversation messages.
        for msg in &core.messages {
            let role = match msg.role {
                CoreRole::User => "user",
                CoreRole::Assistant => "assistant",
                CoreRole::System => "developer",
                CoreRole::Tool => "user",
                _ => "user",
            };

            let text: String = msg
                .content
                .iter()
                .filter_map(|c| match c {
                    CoreContent::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();

            input.push(ResponsesInput {
                role: role.to_owned(),
                content: if text.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::String(text))
                },
            });
        }

        // Tools.
        let tools: Vec<ResponsesTool> = core
            .tools
            .iter()
            .map(|t| ResponsesTool {
                r#type: "function".to_owned(),
                name: Some(t.name.clone()),
                description: t.description.clone(),
                parameters: Some(if t.input_schema.is_null() {
                    serde_json::json!({"type": "object", "properties": {}})
                } else {
                    t.input_schema.clone()
                }),
            })
            .collect();

        // Reasoning effort.
        let reasoning = core
            .sampling
            .reasoning_effort
            .as_ref()
            .map(|effort| ResponsesReasoning {
                effort: Some(effort.clone()),
            });

        let req = ResponsesRequest {
            model: target.upstream_model.clone(),
            input,
            stream: if core.stream { Some(true) } else { None },
            tools,
            reasoning,
        };

        let body = serde_json::to_vec(&req)?;
        let url = expand_url_template(&target.endpoint, target);

        Ok(build_proxy_request(body, target, core.stream, url))
    }

    /// Decode a Responses API response body.
    pub fn decode_response(
        &self,
        bytes: &[u8],
        target: &ProviderAdapterTarget,
    ) -> Result<CoreResponse, ProviderError> {
        let resp: ResponsesResponse = serde_json::from_slice(bytes)?;

        let mut content = Vec::new();

        for output in &resp.output {
            match output.r#type.as_str() {
                "message" => {
                    if let Some(ref content_blocks) = output.content {
                        for block in content_blocks {
                            match block.r#type.as_str() {
                                "output_text" => {
                                    if let Some(ref text) = block.text {
                                        content.push(CoreContent::Text {
                                            text: text.clone(),
                                            cache: None,
                                        });
                                    }
                                }
                                _ => {
                                    if let Some(ref text) = block.text {
                                        content.push(CoreContent::Text {
                                            text: text.clone(),
                                            cache: None,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                "function_call" => {
                    let input = output
                        .arguments
                        .as_ref()
                        .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                    content.push(CoreContent::ToolUse {
                        id: output.call_id.clone().unwrap_or_default(),
                        name: output.name.clone().unwrap_or_default(),
                        input,
                    });
                }
                _ => {}
            }
        }

        // Guarantee at least one content block.
        if content.is_empty() {
            content.push(CoreContent::Text {
                text: String::new(),
                cache: None,
            });
        }

        let has_tool_use = content.iter().any(|c| matches!(c, CoreContent::ToolUse { .. }));
        let stop_reason = if has_tool_use {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };

        let usage = build_responses_usage(&resp.usage);

        Ok(CoreResponse {
            id: Some(resp.id),
            model: response_model_ref(target),
            content,
            stop_reason,
            stop_sequence: None,
            usage,
            provider_meta: serde_json::Map::new(),
        })
    }

    /// Create a new stream decoder.
    pub fn new_stream_decoder(
        &self,
        target: &ProviderAdapterTarget,
    ) -> Box<dyn ProviderStreamDecoder + Send> {
        Box::new(ResponsesStreamDecoder {
            model_ref: response_model_ref(target),
            started: false,
            content_index: 0,
            content_started: false,
            stop_sent: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build usage from Responses API usage.
fn build_responses_usage(usage: &ResponsesUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_tokens: None,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        provenance: UsageProvenance::ProviderReported,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use llm_proxy_core::AuthStyle;
    use llm_proxy_protocol::core::{CoreMessage, CoreTool, SamplingOptions};
    use llm_proxy_protocol::zen::{ResponsesContent, ResponsesOutput};

    fn make_target() -> ProviderAdapterTarget {
        ProviderAdapterTarget {
            provider_name: "test-responses".into(),
            adapter_name: "openai-responses".into(),
            protocol: super::super::ProviderProtocol::OpenAiResponses,
            endpoint: "https://api.openai.com/v1/responses".into(),
            auth_style: AuthStyle::Bearer,
            api_key: "test-key".into(),
            requested_model: "gpt-4o".into(),
            upstream_model: "gpt-4o-2024-08-06".into(),
        }
    }

    fn make_core_request(messages: Vec<CoreMessage>) -> CoreRequest {
        CoreRequest {
            model: ModelRef {
                requested: "gpt-4o".into(),
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
        let adapter = ResponsesAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: ResponsesRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body.model, "gpt-4o-2024-08-06");
        assert_eq!(body.input.len(), 1);
        assert_eq!(body.input[0].role, "user");
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
        let adapter = ResponsesAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: ResponsesRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body.input[0].role, "developer");
    }

    #[test]
    fn encode_tool_declarations() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.tools = vec![CoreTool {
            name: "get_weather".into(),
            description: Some("Get weather".into()),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let adapter = ResponsesAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: ResponsesRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].name, Some("get_weather".to_owned()));
    }

    #[test]
    fn encode_reasoning_effort() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.sampling.reasoning_effort = Some("high".into());
        let adapter = ResponsesAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: ResponsesRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert!(body.reasoning.is_some());
        assert_eq!(body.reasoning.unwrap().effort, Some("high".to_owned()));
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
        let adapter = ResponsesAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: ResponsesRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body.stream, Some(true));
        assert!(proxy_req.stream);
    }

    // -- Decode tests --------------------------------------------------------

    #[test]
    fn decode_text_response() {
        let target = make_target();
        let resp = ResponsesResponse {
            id: "resp_test".into(),
            object: "response".into(),
            created: 12345,
            model: "gpt-4o-2024-08-06".into(),
            output: vec![ResponsesOutput {
                r#type: "message".into(),
                id: Some("msg_1".into()),
                role: Some("assistant".into()),
                content: Some(vec![ResponsesContent {
                    r#type: "output_text".into(),
                    text: Some("hello world".into()),
                }]),
                call_id: None,
                name: None,
                arguments: None,
            }],
            usage: ResponsesUsage {
                input_tokens: 100,
                output_tokens: 50,
            },
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let adapter = ResponsesAdapter;
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.id, Some("resp_test".to_owned()));
        assert_eq!(core_resp.stop_reason, StopReason::EndTurn);
        assert_eq!(core_resp.usage.input_tokens, 100);
        assert_eq!(core_resp.content.len(), 1);
    }

    #[test]
    fn decode_function_call_response() {
        let target = make_target();
        let resp = ResponsesResponse {
            id: "resp_test".into(),
            object: "response".into(),
            created: 0,
            model: "gpt-4o".into(),
            output: vec![ResponsesOutput {
                r#type: "function_call".into(),
                id: Some("fc_1".into()),
                role: None,
                content: None,
                call_id: Some("call_123".into()),
                name: Some("get_weather".into()),
                arguments: Some(r#"{"city":"SF"}"#.into()),
            }],
            usage: ResponsesUsage {
                input_tokens: 200,
                output_tokens: 80,
            },
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let adapter = ResponsesAdapter;
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.stop_reason, StopReason::ToolUse);
        match &core_resp.content[0] {
            CoreContent::ToolUse { id, name, input } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "SF");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn decode_preserves_requested_model() {
        let mut target = make_target();
        target.requested_model = "my-model".into();

        let resp = ResponsesResponse {
            id: "test".into(),
            object: "response".into(),
            created: 0,
            model: "gpt-4o-2024-08-06".into(),
            output: vec![ResponsesOutput {
                r#type: "message".into(),
                id: None,
                role: Some("assistant".into()),
                content: Some(vec![ResponsesContent {
                    r#type: "output_text".into(),
                    text: Some("hi".into()),
                }]),
                call_id: None,
                name: None,
                arguments: None,
            }],
            usage: ResponsesUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let adapter = ResponsesAdapter;
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.model.requested, "my-model");
    }

    // -- Streaming tests -----------------------------------------------------

    #[test]
    fn stream_text_decoding() {
        let target = make_target();
        let adapter = ResponsesAdapter;
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"type":"response.created","id":"resp_1"}"#),
            make_frame(r#"{"type":"response.output_text.delta","delta":"Hello"}"#),
            make_frame(r#"{"type":"response.output_text.delta","delta":" world"}"#),
            make_frame(r#"{"type":"response.completed","usage":{"input_tokens":10,"output_tokens":5}}"#),
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
        assert!(all_events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })));
    }

    #[test]
    fn stream_done_marker_ignored() {
        let target = make_target();
        let adapter = ResponsesAdapter;
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame("[DONE]");
        let events = decoder.decode_frame(&frame).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn stream_finish_emits_lifecycle() {
        let target = make_target();
        let adapter = ResponsesAdapter;
        let mut decoder = adapter.new_stream_decoder(&target);

        let events = decoder.finish().unwrap();
        assert!(events.iter().any(|e| matches!(e, CoreEvent::MessageStart { .. })));
        assert!(events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })));
    }

    // -- Source guard ---------------------------------------------------------

    #[test]
    fn adapter_source_no_forbidden_imports() {
        let source = include_str!("responses.rs");
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
