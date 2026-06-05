//! OpenAI Chat Completions API types.
//!
//! Covers request/response structures for the `/v1/chat/completions` endpoint,
//! including streaming chunks and error payloads.
//!
//! Reference: <https://platform.openai.com/docs/api-reference/chat>

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Cache control (shared with the Anthropic module)
// ---------------------------------------------------------------------------

/// Cache-control directive attached to a message or content block.
///
/// Anthropic models accept `cache_control` on messages; the proxy
/// preserves the annotation when translating between formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    /// Discriminator – typically `"ephemeral"`.
    pub r#type: String,
}

// ---------------------------------------------------------------------------
// Streaming options
// ---------------------------------------------------------------------------

/// Controls extra metadata returned alongside streaming chunks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOptions {
    /// When `true`, the final chunk includes a [`UsageInfo`] object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// A tool (function) that the model may invoke during generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    /// Always `"function"` for function-calling tools.
    pub r#type: String,
    /// Schema of the function the model can call.
    pub function: FunctionDef,
}

/// Describes a callable function, including its JSON-schema parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    /// Name the model uses to reference this function.
    pub name: String,
    /// Human-readable description that helps the model decide when to call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema object describing the function parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Function / tool calls inside messages
// ---------------------------------------------------------------------------

/// A single function invocation produced by the model.
///
/// In streaming delta chunks both `id` and `function` may be absent;
/// the server sends partial `ToolCall`s that the client merges by `index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Position inside the `tool_calls` array (streaming deltas only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<i32>,
    /// Stable identifier for this tool call (absent in deltas).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Always `"function"` when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Function name and arguments (absent in the very first delta).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionCall>,
}

/// Name + arguments of a function call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// The function name chosen by the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// JSON-encoded argument object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// Chat message
// ---------------------------------------------------------------------------

/// A single message in a conversation.
///
/// For assistant messages that carry tool calls, `content` may be empty
/// while `tool_calls` is populated. For tool-result messages, `tool_call_id`
/// identifies the call being answered.
///
/// Note: `deny_unknown_fields` is intentionally omitted because this struct is
/// used in streaming deltas where fields may vary between providers. Unknown
/// fields are silently ignored rather than causing parse failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"`, `"assistant"`, or `"tool"`.
    ///
    /// Defaults to empty when absent (streaming deltas often omit this).
    #[serde(default)]
    pub role: String,
    /// The text body of the message.
    ///
    /// Defaults to empty when absent (streaming deltas often omit this).
    #[serde(default)]
    pub content: String,
    /// Chain-of-thought text emitted by reasoning models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Tool calls emitted by an assistant message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Sender name (used when `role` is `"tool"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Links this tool-result message to a specific [`ToolCall::id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Anthropic-style cache hint preserved across protocol translation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ---------------------------------------------------------------------------
// Chat completion request
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/chat/completions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionRequest {
    /// Model identifier (e.g. `"gpt-4o"`, `"glm-5.1"`).
    pub model: String,
    /// Conversation turns sent to the model.
    pub messages: Vec<ChatMessage>,
    /// Request a streaming response when `Some(true)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Sampling temperature in `[0, 2]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Upper bound on tokens generated in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// Reasoning effort level (e.g. `"low"`, `"medium"`, `"high"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Extended thinking configuration (provider-specific JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// Tools the model is allowed to call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    /// Controls which (if any) tool the model must call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Stop sequences (string or array of strings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<serde_json::Value>,
    /// Extra streaming metadata flags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

// ---------------------------------------------------------------------------
// Token usage
// ---------------------------------------------------------------------------

/// Token usage statistics returned by the API.
///
/// Note: `deny_unknown_fields` is intentionally omitted because providers may
/// return additional usage fields (e.g. `prompt_tokens_details`) that the
/// proxy does not model. Unknown fields are silently ignored rather than
/// causing parse failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    /// Tokens consumed by the prompt.
    pub prompt_tokens: i32,
    /// Tokens produced by the completion.
    pub completion_tokens: i32,
    /// `prompt_tokens + completion_tokens`.
    pub total_tokens: i32,
    /// Prompt tokens served from a cache (Anthropic / provider-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<i32>,
    /// Prompt tokens that missed the cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<i32>,
}

// ---------------------------------------------------------------------------
// Choice
// ---------------------------------------------------------------------------

/// A single completion alternative inside a response or chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// Position of this choice in the array.
    pub index: i32,
    /// The full assistant message (non-streaming responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<ChatMessage>,
    /// Why the model stopped (e.g. `"stop"`, `"tool_calls"`, `"length"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Partial message delta (streaming chunks only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<ChatMessage>,
}

// ---------------------------------------------------------------------------
// Non-streaming response
// ---------------------------------------------------------------------------

/// Response body for a non-streaming Chat Completions call.
///
/// Note: `deny_unknown_fields` is intentionally omitted on upstream response
/// types. Providers may add fields (e.g. `service_tier`, `system_fingerprint`)
/// that the proxy does not model. Without `deny_unknown_fields`, these fields
/// are silently ignored during deserialization rather than causing a parse
/// failure that would surface as a 502 to the client. Request types retain
/// `deny_unknown_fields` for strict inbound validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// Unique completion identifier.
    pub id: String,
    /// Object type, always `"chat.completion"`.
    pub object: String,
    /// Unix timestamp of creation.
    pub created: i64,
    /// Model that generated the response.
    pub model: String,
    /// Ordered list of completion alternatives.
    pub choices: Vec<Choice>,
    /// Token usage for this request.
    pub usage: UsageInfo,
}

// ---------------------------------------------------------------------------
// Streaming chunk
// ---------------------------------------------------------------------------

/// A single Server-Sent Events chunk for a streaming completion.
///
/// Note: `deny_unknown_fields` is intentionally omitted here because upstream
/// providers may add new fields to streaming chunks at any time. With
/// `deny_unknown_fields`, serde would reject unknown fields, causing the chunk
/// to be silently dropped. Without it, unknown fields are simply ignored,
/// avoiding silent data loss. Request types (e.g. `ChatCompletionRequest`)
/// retain `deny_unknown_fields` because the proxy controls their construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    /// Unique completion identifier (stable across all chunks).
    pub id: String,
    /// Object type, always `"chat.completion.chunk"`.
    pub object: String,
    /// Unix timestamp of creation.
    pub created: i64,
    /// Model that generated the chunk.
    pub model: String,
    /// Ordered list of delta choices.
    pub choices: Vec<Choice>,
    /// Token usage (present only on the final chunk when `include_usage` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Top-level error envelope returned by OpenAI-compatible APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Nested error details.
    pub error: ErrorDetails,
}

/// Detailed error information inside an [`ErrorResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetails {
    /// Machine-readable error category (e.g. `"invalid_request_error"`).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Human-readable error description.
    pub message: String,
    /// Optional error code (e.g. `"invalid_api_key"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
