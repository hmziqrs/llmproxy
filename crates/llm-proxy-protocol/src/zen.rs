//! Responses API and Gemini API request/response types.
//!
//! These types model the wire format for two LLM provider families:
//!
//! - **Responses API** – the OpenAI-style Responses endpoint used for
//!   chat completions with optional tool use and streaming.
//! - **Gemini API** – the Google Gemini generate-content endpoint,
//!   including streaming chunk support.
//!
//! Every struct derives `Debug`, `Clone`, `Serialize`, and `Deserialize`
//! so they can be serialized/deserialized directly via `serde_json`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Responses API types
// ---------------------------------------------------------------------------

/// Top-level request body for the Responses API.
///
/// Note: `deny_unknown_fields` is intentionally omitted because this is an
/// outbound-only type -- the proxy constructs it internally via
/// `transform_to_responses()` and sends it to upstream providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesRequest {
    /// Model identifier (e.g. `"gpt-4o"`).
    pub model: String,
    /// Conversation turns sent to the model.
    pub input: Vec<ResponsesInput>,
    /// When `Some(true)` the server streams chunked responses.
    pub stream: Option<bool>,
    /// Tools available to the model during generation.
    pub tools: Vec<ResponsesTool>,
    /// Optional reasoning / chain-of-thought configuration.
    pub reasoning: Option<ResponsesReasoning>,
    /// Controls which (if any) tool the model must call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

/// A single message in the conversation input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesInput {
    /// Speaker role (e.g. `"user"`, `"assistant"`).
    pub role: String,
    /// Arbitrary JSON content payload.
    pub content: Option<serde_json::Value>,
}

/// A tool definition offered to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesTool {
    /// Tool variant discriminator (e.g. `"function"`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Name of the tool.
    pub name: Option<String>,
    /// Human-readable description of what the tool does.
    pub description: Option<String>,
    /// JSON Schema describing the tool's parameters.
    pub parameters: Option<serde_json::Value>,
}

/// Reasoning effort configuration for supported models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesReasoning {
    /// Effort level (e.g. `"low"`, `"medium"`, `"high"`).
    pub effort: Option<String>,
}

/// Non-streaming response from the Responses API.
///
/// Note: `deny_unknown_fields` is intentionally omitted on upstream response
/// types. Providers may add fields that the proxy does not model; unknown
/// fields are silently ignored rather than causing parse failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesResponse {
    /// Unique response identifier.
    pub id: String,
    /// Object type discriminator (e.g. `"response"`).
    pub object: String,
    /// Unix timestamp (seconds) of creation.
    pub created: i64,
    /// Model that produced the response.
    pub model: String,
    /// Ordered list of output items (messages, tool calls, etc.).
    pub output: Vec<ResponsesOutput>,
    /// Token usage statistics.
    pub usage: ResponsesUsage,
    /// Response status (e.g. `"completed"`, `"failed"`, `"incomplete"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// A single output item in a Responses API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesOutput {
    /// Item type discriminator (e.g. `"message"`, `"function_call"`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Unique item identifier.
    pub id: Option<String>,
    /// Speaker role (present for message items).
    pub role: Option<String>,
    /// Content blocks (present for message items).
    pub content: Option<Vec<ResponsesContent>>,
    /// Function call identifier (present for function-call items).
    pub call_id: Option<String>,
    /// Function name (present for function-call items).
    pub name: Option<String>,
    /// JSON-encoded function arguments (present for function-call items).
    pub arguments: Option<String>,
}

/// A content block inside a Responses API output message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesContent {
    /// Content type discriminator (e.g. `"output_text"`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// The text payload.
    pub text: Option<String>,
}

/// Token usage reported by the Responses API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsage {
    /// Number of tokens in the prompt.
    pub input_tokens: i32,
    /// Number of tokens in the completion.
    pub output_tokens: i32,
}

/// A single chunk in a streaming Responses API response.
///
/// Note: `deny_unknown_fields` is intentionally omitted on streaming chunk
/// types. Upstream providers may add new event fields at any time; unknown
/// fields are silently ignored rather than causing chunk drops. The `delta`
/// field carries incremental text for `response.output_text.delta` events.
/// For `response.function_call_arguments.delta` events, the delta carries
/// incremental JSON arguments for tool calls. Parallel function calls have
/// their argument deltas interleaved and disambiguated by `output_index`; the
/// Responses API provider adapter tracks open blocks per `output_index` and
/// emits the full `CoreEvent` tool call lifecycle
/// (`ToolCallStart`/`ToolCallDelta`/`ToolCallStop`) for each.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesChunk {
    /// Chunk type discriminator.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Response / item identifier.
    pub id: Option<String>,
    /// Incremental text delta.
    pub delta: Option<String>,
    /// Complete output items (emitted at certain events).
    pub output: Option<Vec<ResponsesOutput>>,
    /// Token usage (usually present on the final chunk).
    pub usage: Option<ResponsesUsage>,
    /// Error object present on `response.failed` events.
    pub error: Option<serde_json::Value>,
    /// Upstream output-item index carried on `response.output_item.added`,
    /// `response.function_call_arguments.delta`, and
    /// `response.function_call_arguments.done` events.
    ///
    /// The Responses API interleaves parallel function-call argument deltas and
    /// disambiguates them via this index. Defaults to `None` so legacy chunks
    /// that predate the field (and non-indexed events like `response.created`)
    /// still parse. When absent on a function-call event the adapter falls back
    /// to treating it as a single serial call.
    #[serde(default)]
    pub output_index: Option<usize>,
}

// ---------------------------------------------------------------------------
// Gemini API types
// ---------------------------------------------------------------------------

/// Top-level request body for the Gemini generate-content endpoint.
///
/// Note: `deny_unknown_fields` is intentionally omitted for outbound-only
/// request structs. The proxy constructs these internally and never
/// deserializes them from external input, so unknown field protection
/// provides no benefit and would be overly restrictive if the proxy adds
/// passthrough fields in the future.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiRequest {
    /// Conversation contents to send to the model.
    pub contents: Vec<GeminiContent>,
    /// Optional generation parameters.
    pub generation_config: Option<GeminiGenerationConfig>,
    /// Tools available to the model.
    pub tools: Vec<GeminiTool>,
    /// When `Some(true)` the server streams chunked responses.
    ///
    /// Note: The Gemini API typically accepts stream as a query parameter
    /// (`?alt=sse`) rather than a body field. This field is kept for internal
    /// bookkeeping by the adapter, which should translate it to the appropriate
    /// query parameter when constructing the HTTP request.
    pub stream: Option<bool>,
}

/// A single message in a Gemini conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiContent {
    /// Speaker role (e.g. `"user"`, `"model"`).
    pub role: String,
    /// Ordered content parts within this message.
    ///
    /// Defaults to an empty vec when omitted. The Gemini API may return a
    /// candidate with no `parts` (e.g. empty content blocks), so this field is
    /// deserialized with a default rather than treated as required.
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

/// A single content part inside a Gemini message.
///
/// Note: `deny_unknown_fields` is intentionally omitted because Gemini may
/// return additional part types (e.g. `inline_data`, `file_data`) that the
/// proxy does not model yet. Unknown fields are silently ignored rather than
/// causing parse failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GeminiPart {
    /// Text content of this part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Function call (present when the model invokes a tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall>,
    /// Function response (present when sending a tool result back to the model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GeminiFunctionResponse>,
}

/// A function call emitted by the Gemini model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionCall {
    /// The name of the function to call.
    pub name: String,
    /// The arguments to pass to the function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// A function response sent back to the model as a tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionResponse {
    /// The name of the function that was called.
    pub name: String,
    /// The response payload from the function.
    pub response: serde_json::Value,
}

impl GeminiPart {
    /// Create a text part.
    pub fn text(text: String) -> Self {
        Self {
            text: Some(text),
            function_call: None,
            function_response: None,
        }
    }

    /// Create a function call part.
    pub fn function_call(name: String, args: Option<serde_json::Value>) -> Self {
        Self {
            text: None,
            function_call: Some(GeminiFunctionCall { name, args }),
            function_response: None,
        }
    }

    /// Create a function response part.
    pub fn function_response(name: String, response: serde_json::Value) -> Self {
        Self {
            text: None,
            function_call: None,
            function_response: Some(GeminiFunctionResponse { name, response }),
        }
    }
}

/// Generation parameters for a Gemini request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiGenerationConfig {
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold.
    pub top_p: Option<f64>,
    /// Maximum number of tokens in the completion.
    pub max_output_tokens: Option<i32>,
    /// Stop sequences that cause generation to stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
}

/// A tool declaration in the Gemini API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiTool {
    /// Function declarations exposed to the model.
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

/// A single function declaration inside a Gemini tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionDeclaration {
    /// Function name.
    pub name: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// JSON Schema describing the function parameters.
    pub parameters: Option<serde_json::Value>,
}

/// Non-streaming response from the Gemini generate-content endpoint.
///
/// Note: `deny_unknown_fields` is intentionally omitted on upstream response
/// types. Providers may add fields that the proxy does not model; unknown
/// fields are silently ignored rather than causing parse failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiResponse {
    /// Candidate completions.
    pub candidates: Vec<GeminiCandidate>,
    /// Token usage metadata.
    #[serde(rename = "usageMetadata")]
    pub usage_metadata: Option<GeminiUsage>,
}

/// A single candidate in a Gemini response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiCandidate {
    /// The generated content.
    ///
    /// This is optional because real Gemini API responses routinely omit
    /// `content` entirely for candidates blocked by `SAFETY`/`RECITATION` or
    /// for prompt-feedback-only responses. A missing `content` is treated as
    /// producing no text parts; the stop reason is derived from
    /// `finish_reason` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<GeminiContent>,
    /// Reason the model stopped generating (e.g. `"STOP"`).
    #[serde(rename = "finishReason")]
    pub finish_reason: Option<String>,
}

/// Token usage metadata reported by the Gemini API.
///
/// Uses manual `#[serde(rename)]` attributes for consistency with other Gemini
/// types in this module, rather than `rename_all = "camelCase"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiUsage {
    /// Number of tokens in the prompt.
    #[serde(rename = "promptTokenCount")]
    pub prompt_token_count: i32,
    /// Number of tokens across all candidates.
    #[serde(rename = "candidatesTokenCount")]
    pub candidates_token_count: i32,
    /// Total tokens (prompt + candidates).
    #[serde(rename = "totalTokenCount")]
    pub total_token_count: i32,
}

/// A single chunk in a streaming Gemini response.
///
/// Note: `deny_unknown_fields` is intentionally omitted on streaming chunk
/// types. Upstream providers may add new fields at any time; unknown fields
/// are silently ignored rather than causing chunk drops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiStreamChunk {
    /// Candidate completions in this chunk.
    pub candidates: Vec<GeminiCandidate>,
    /// Token usage metadata (usually present on the final chunk).
    #[serde(rename = "usageMetadata")]
    pub usage_metadata: Option<GeminiUsage>,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_request_round_trip() {
        let req = ResponsesRequest {
            model: "gpt-4o".into(),
            input: vec![ResponsesInput {
                role: "user".into(),
                content: Some(serde_json::json!("hello")),
            }],
            stream: Some(true),
            tools: vec![],
            reasoning: None,
            tool_choice: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ResponsesRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model, "gpt-4o");
        assert_eq!(back.input.len(), 1);
    }

    #[test]
    fn gemini_request_round_trip() {
        let req = GeminiRequest {
            contents: vec![GeminiContent {
                role: "user".into(),
                parts: vec![GeminiPart::text("hello".into())],
            }],
            generation_config: Some(GeminiGenerationConfig {
                temperature: Some(0.7),
                top_p: None,
                max_output_tokens: Some(1024),
                stop_sequences: None,
            }),
            tools: vec![],
            stream: Some(true),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: GeminiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.contents.len(), 1);
        assert!(back.generation_config.is_some());
    }

    #[test]
    fn gemini_response_round_trip() {
        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: "model".into(),
                    parts: vec![GeminiPart::text("hi there".into())],
                }),
                finish_reason: Some("STOP".into()),
            }],
            usage_metadata: Some(GeminiUsage {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: GeminiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.candidates.len(), 1);
        let usage = back.usage_metadata.unwrap();
        assert_eq!(usage.prompt_token_count, 10);
        assert_eq!(usage.total_token_count, 15);
    }

    #[test]
    fn gemini_usage_camel_case_serialization() {
        let usage = GeminiUsage {
            prompt_token_count: 100,
            candidates_token_count: 50,
            total_token_count: 150,
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(
            json.contains("promptTokenCount"),
            "expected camelCase: {json}"
        );
        assert!(
            json.contains("candidatesTokenCount"),
            "expected camelCase: {json}"
        );
        assert!(
            json.contains("totalTokenCount"),
            "expected camelCase: {json}"
        );
    }

    #[test]
    fn responses_chunk_round_trip() {
        let chunk = ResponsesChunk {
            r#type: "response.output_text.delta".into(),
            id: Some("resp_123".into()),
            delta: Some("hello".into()),
            output: None,
            usage: None,
            error: None,
            output_index: None,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let back: ResponsesChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(back.delta.as_deref(), Some("hello"));
    }

    #[test]
    fn responses_chunk_output_index_defaulted_when_absent() {
        // Legacy chunks that predate `output_index` must still parse into None.
        let json = r#"{"type":"response.created","id":"resp_1"}"#;
        let back: ResponsesChunk = serde_json::from_str(json).unwrap();
        assert!(back.output_index.is_none());
    }

    #[test]
    fn responses_chunk_output_index_parsed_when_present() {
        let json = r#"{"type":"response.output_item.added","output_index":3}"#;
        let back: ResponsesChunk = serde_json::from_str(json).unwrap();
        assert_eq!(back.output_index, Some(3));
    }

    #[test]
    fn gemini_stream_chunk_round_trip() {
        let chunk = GeminiStreamChunk {
            candidates: vec![],
            usage_metadata: None,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let back: GeminiStreamChunk = serde_json::from_str(&json).unwrap();
        assert!(back.candidates.is_empty());
    }
}
