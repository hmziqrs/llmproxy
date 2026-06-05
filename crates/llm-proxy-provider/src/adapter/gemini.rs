//! Google Gemini GenerateContent API provider adapter.
//!
//! Translates between core types and the Gemini wire format:
//!
//! ```text
//! CoreRequest  -> GeminiRequest
//! GeminiResponse -> CoreResponse
//! GeminiStreamChunk stream -> CoreEvent stream
//! ```

use llm_proxy_protocol::core::{
    ContentKind, CoreContent, CoreEvent, CoreRequest, CoreResponse, CoreRole,
    ModelRef, StopReason, Usage, UsageProvenance,
};
use llm_proxy_protocol::zen::{
    GeminiFunctionDeclaration, GeminiGenerationConfig,
    GeminiRequest, GeminiResponse, GeminiStreamChunk, GeminiTool, GeminiUsage,
};

use super::{
    build_proxy_request, expand_url_template, map_gemini_finish_reason, response_model_ref,
    ProviderAdapterTarget, ProviderStreamDecoder,
};
use crate::error::ProviderError;
use crate::sse::SseFrame;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Adapter for the Google Gemini GenerateContent API.
#[derive(Debug, Clone)]
pub struct GeminiAdapter;

// ---------------------------------------------------------------------------
// Stream decoder
// ---------------------------------------------------------------------------

/// Stateful stream decoder for Gemini SSE frames.
#[derive(Debug)]
pub struct GeminiStreamDecoder {
    model_ref: ModelRef,
    started: bool,
    content_index: usize,
    content_started: bool,
    tool_started: bool,
    /// Maps Gemini candidate index to tool content block index.
    tool_blocks: Vec<usize>,
    stop_sent: bool,
}

impl ProviderStreamDecoder for GeminiStreamDecoder {
    fn decode_frame(&mut self, frame: &SseFrame) -> Result<Vec<CoreEvent>, ProviderError> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(vec![]);
        }

        let chunk: GeminiStreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(data, "malformed Gemini chunk, skipping");
                return Ok(vec![]);
            }
        };

        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            events.push(CoreEvent::MessageStart {
                id: None,
                model: self.model_ref.clone(),
            });
        }

        // Usage-only chunk (no candidates).
        if chunk.candidates.is_empty() {
            if let Some(ref usage) = chunk.usage_metadata {
                events.push(CoreEvent::UsageDelta {
                    usage: build_gemini_usage(usage),
                });
            }
            return Ok(events);
        }

        let candidate = &chunk.candidates[0];

        // Process parts.
        for part in &candidate.content.parts {
            if let Some(ref text) = part.text {
                if !text.is_empty() {
                    if !self.content_started {
                        self.content_started = true;
                        events.push(CoreEvent::ContentStart {
                            index: self.content_index,
                            kind: ContentKind::Text,
                        });
                    }
                    events.push(CoreEvent::TextDelta {
                        index: self.content_index,
                        text: text.clone(),
                    });
                }
            }

            // Handle function call parts.
            if let Some(ref function_call) = part.function_call {
                self.close_content_if_open(&mut events);

                let block_idx = self.content_index;
                self.tool_blocks.push(block_idx);
                self.tool_started = true;

                events.push(CoreEvent::ToolCallStart {
                    index: block_idx,
                    id: format!("gemini_call_{}", block_idx),
                    name: function_call.name.clone(),
                });

                if let Some(ref args) = function_call.args {
                    let args_str = serde_json::to_string(args).unwrap_or_default();
                    if !args_str.is_empty() && args_str != "null" {
                        events.push(CoreEvent::ToolCallDelta {
                            index: block_idx,
                            args_delta: args_str,
                        });
                    }
                }

                events.push(CoreEvent::ToolCallStop { index: block_idx });
                self.tool_started = false;
                self.content_index += 1;
            }
        }

        // Handle finish reason.
        if let Some(ref reason) = candidate.finish_reason {
            if !reason.is_empty() && !self.stop_sent {
                self.close_content_if_open(&mut events);

                let stop_reason = map_gemini_finish_reason(reason);

                if let Some(ref usage) = chunk.usage_metadata {
                    events.push(CoreEvent::UsageDelta {
                        usage: build_gemini_usage(usage),
                    });
                }

                events.push(CoreEvent::MessageStop {
                    stop_reason,
                    stop_sequence: None,
                });
                self.stop_sent = true;
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

impl GeminiStreamDecoder {
    fn close_content_if_open(&mut self, _events: &mut Vec<CoreEvent>) {
        if self.content_started {
            self.content_started = false;
            self.content_index += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// GeminiAdapter impl
// ---------------------------------------------------------------------------

impl GeminiAdapter {
    /// Encode a core request into a Gemini GenerateContent request.
    pub fn encode_request(
        &self,
        core: &CoreRequest,
        target: &ProviderAdapterTarget,
    ) -> Result<super::ProxyRequest, ProviderError> {
        let mut contents = Vec::new();

        // System instructions are handled via generation config or prepended.
        // Gemini does not have a dedicated system field in the basic request.

        for msg in &core.messages {
            let role = match msg.role {
                CoreRole::User => "user",
                CoreRole::Assistant => "model",
                CoreRole::System => "user", // Gemini uses "user" for system prompts.
                CoreRole::Tool => "user",   // Tool results go as user.
                _ => "user",
            };

            let text: String = msg
                .content
                .iter()
                .filter_map(|c| match c {
                    CoreContent::Text { text, .. } => Some(text.clone()),
                    CoreContent::ToolResult {
                        content: result_content,
                        ..
                    } => {
                        let result_text: String = result_content
                            .iter()
                            .filter_map(|c| match c {
                                CoreContent::Text { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        if result_text.is_empty() {
                            None
                        } else {
                            Some(result_text)
                        }
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            if !text.is_empty() {
                let val = serde_json::json!({"role": role, "parts": [{"text": text}]});
                contents.push(serde_json::from_value(val)?);
            }
        }

        // Prepend system prompt as user message if present.
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
                let sys_val = serde_json::json!({"role": "user", "parts": [{"text": system_text}]});
                contents.insert(0, serde_json::from_value(sys_val)?);
                // Add a model acknowledgment.
                let ack_val = serde_json::json!({"role": "model", "parts": [{"text": "Understood."}]});
                contents.insert(1, serde_json::from_value(ack_val)?);
            }
        }

        // Tools.
        let tools: Vec<GeminiTool> = if core.tools.is_empty() {
            vec![]
        } else {
            vec![GeminiTool {
                function_declarations: core
                    .tools
                    .iter()
                    .map(|t| {
                        let schema = if t.input_schema.is_null() {
                            serde_json::json!({"type": "object", "properties": {}})
                        } else {
                            t.input_schema.clone()
                        };
                        GeminiFunctionDeclaration {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: Some(schema),
                        }
                    })
                    .collect(),
            }]
        };

        // Generation config.
        let generation_config = GeminiGenerationConfig {
            temperature: core.sampling.temperature,
            max_output_tokens: core.sampling.max_tokens,
        };

        let req = GeminiRequest {
            contents,
            generation_config: Some(generation_config),
            tools,
            stream: if core.stream { Some(true) } else { None },
        };

        let body = serde_json::to_vec(&req)?;
        let url = expand_url_template(&target.endpoint, target);

        Ok(build_proxy_request(body, target, core.stream, url))
    }

    /// Decode a Gemini GenerateContent response body.
    pub fn decode_response(
        &self,
        bytes: &[u8],
        target: &ProviderAdapterTarget,
    ) -> Result<CoreResponse, ProviderError> {
        let resp: GeminiResponse = serde_json::from_slice(bytes)?;

        if resp.candidates.is_empty() {
            return Err(ProviderError::SseFraming(
                "no candidates in response".to_owned(),
            ));
        }

        let candidate = &resp.candidates[0];
        let mut content = Vec::new();

        for part in &candidate.content.parts {
            if let Some(ref text) = part.text {
                if !text.is_empty() {
                    content.push(CoreContent::Text {
                        text: text.clone(),
                        cache: None,
                    });
                }
            }
            if let Some(ref function_call) = part.function_call {
                let input = function_call
                    .args
                    .clone()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                content.push(CoreContent::ToolUse {
                    id: format!("gemini_call_{}", content.len()),
                    name: function_call.name.clone(),
                    input,
                });
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
        let stop_reason = candidate
            .finish_reason
            .as_deref()
            .map(map_gemini_finish_reason)
            .unwrap_or(if has_tool_use {
                StopReason::ToolUse
            } else {
                StopReason::Unknown
            });

        let usage = resp
            .usage_metadata
            .as_ref()
            .map(build_gemini_usage)
            .unwrap_or_else(Usage::synthetic_zero);

        Ok(CoreResponse {
            id: None,
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
        Box::new(GeminiStreamDecoder {
            model_ref: response_model_ref(target),
            started: false,
            content_index: 0,
            content_started: false,
            tool_started: false,
            tool_blocks: Vec::new(),
            stop_sent: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build usage from Gemini usage metadata.
fn build_gemini_usage(usage: &GeminiUsage) -> Usage {
    Usage {
        input_tokens: usage.prompt_token_count,
        output_tokens: usage.candidates_token_count,
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
    use llm_proxy_protocol::core::{CoreMessage, CoreTool, SamplingOptions};

    fn make_target() -> ProviderAdapterTarget {
        ProviderAdapterTarget {
            provider_name: "test-gemini".into(),
            adapter_name: "gemini".into(),
            protocol: super::super::ProviderProtocol::GeminiGenerateContent,
            endpoint: "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent".into(),
            auth_style: llm_proxy_core::AuthStyle::Bearer,
            api_key: "test-key".into(),
            requested_model: "gemini-2.5-pro".into(),
            upstream_model: "gemini-2.5-pro".into(),
        }
    }

    fn make_core_request(messages: Vec<CoreMessage>) -> CoreRequest {
        CoreRequest {
            model: ModelRef {
                requested: "gemini-2.5-pro".into(),
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
        let adapter = GeminiAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: GeminiRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body.contents.len(), 1);
        assert_eq!(body.contents[0].role, "user");
        assert_eq!(body.contents[0].parts[0].text, Some("Hello".to_owned()));
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
        let adapter = GeminiAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: GeminiRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        // System prompt should be prepended as user + model acknowledgment.
        assert_eq!(body.contents[0].role, "user");
        assert_eq!(body.contents[0].parts[0].text, Some("You are helpful".to_owned()));
        assert_eq!(body.contents[1].role, "model");
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
        let adapter = GeminiAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: GeminiRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].function_declarations[0].name, "get_weather");
    }

    #[test]
    fn encode_generation_config() {
        let mut core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        core.sampling = SamplingOptions {
            temperature: Some(0.7),
            max_tokens: Some(1024),
            ..Default::default()
        };
        let adapter = GeminiAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        let body: GeminiRequest = serde_json::from_slice(&proxy_req.body).unwrap();
        let config = body.generation_config.unwrap();
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.max_output_tokens, Some(1024));
    }

    #[test]
    fn encode_url_template_expansion() {
        let core = make_core_request(vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hi".into(),
                cache: None,
            }],
        }]);
        let adapter = GeminiAdapter;
        let target = make_target();
        let proxy_req = adapter.encode_request(&core, &target).unwrap();

        assert_eq!(
            proxy_req.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    // -- Decode tests --------------------------------------------------------

    #[test]
    fn decode_text_response() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "hello world"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50,
                "totalTokenCount": 150
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = GeminiAdapter;
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.model.requested, "gemini-2.5-pro");
        assert_eq!(core_resp.stop_reason, StopReason::EndTurn);
        assert_eq!(core_resp.usage.input_tokens, 100);
        assert_eq!(core_resp.usage.output_tokens, 50);
        assert_eq!(core_resp.content.len(), 1);
    }

    #[test]
    fn decode_function_call_response() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"function_call": {"name": "get_weather", "args": {"city": "SF"}}}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 200,
                "candidatesTokenCount": 80,
                "totalTokenCount": 280
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = GeminiAdapter;
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        match &core_resp.content[0] {
            CoreContent::ToolUse { name, input, .. } => {
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "SF");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn decode_no_candidates_error() {
        let target = make_target();
        let resp_json = serde_json::json!({
            "candidates": [],
            "usageMetadata": null
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = GeminiAdapter;
        let result = adapter.decode_response(&bytes, &target);
        assert!(result.is_err());
    }

    #[test]
    fn decode_preserves_requested_model() {
        let mut target = make_target();
        target.requested_model = "my-gemini".into();

        let resp_json = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "hi"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        });
        let bytes = serde_json::to_vec(&resp_json).unwrap();
        let adapter = GeminiAdapter;
        let core_resp = adapter.decode_response(&bytes, &target).unwrap();

        assert_eq!(core_resp.model.requested, "my-gemini");
    }

    // -- Streaming tests -----------------------------------------------------

    #[test]
    fn stream_text_decoding() {
        let target = make_target();
        let adapter = GeminiAdapter;
        let mut decoder = adapter.new_stream_decoder(&target);

        let frames = vec![
            make_frame(r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]},"finishReason":null}],"usageMetadata":null}"#),
            make_frame(r#"{"candidates":[{"content":{"role":"model","parts":[{"text":" world"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#),
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
        let adapter = GeminiAdapter;
        let mut decoder = adapter.new_stream_decoder(&target);

        let frame = make_frame("[DONE]");
        let events = decoder.decode_frame(&frame).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn stream_finish_emits_lifecycle() {
        let target = make_target();
        let adapter = GeminiAdapter;
        let mut decoder = adapter.new_stream_decoder(&target);

        let events = decoder.finish().unwrap();
        assert!(events.iter().any(|e| matches!(e, CoreEvent::MessageStart { .. })));
        assert!(events.iter().any(|e| matches!(e, CoreEvent::MessageStop { .. })));
    }

    // -- Source guard ---------------------------------------------------------

    #[test]
    fn adapter_source_no_forbidden_imports() {
        let source = include_str!("gemini.rs");
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
