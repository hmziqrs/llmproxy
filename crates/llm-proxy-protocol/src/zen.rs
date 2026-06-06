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
//! so they can be used directly as Axum extractors / response bodies via
//! `axum-serde`.

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
/// For `response.function_call_arguments.delta` events, the delta is also a
/// string -- this is a known gap: the stream decoding layer does not currently
/// handle function call streaming for the Responses API (see
/// `responses_function_call_stream` test). Phase 2/5/12 should add the right
/// adapter fixtures for this.
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
    pub stream: Option<bool>,
}

/// A single message in a Gemini conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiContent {
    /// Speaker role (e.g. `"user"`, `"model"`).
    pub role: String,
    /// Ordered content parts within this message.
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
    pub content: GeminiContent,
    /// Reason the model stopped generating (e.g. `"STOP"`).
    #[serde(rename = "finishReason")]
    pub finish_reason: Option<String>,
}

/// Token usage metadata reported by the Gemini API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsage {
    /// Number of tokens in the prompt.
    pub prompt_token_count: i32,
    /// Number of tokens across all candidates.
    pub candidates_token_count: i32,
    /// Total tokens (prompt + candidates).
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
