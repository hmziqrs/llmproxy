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
/// This is a separate type from [`crate::core::CacheControl`] by design:
/// the OpenAI wire format uses a plain string for the `type` field, while
/// the core type uses a proper [`CacheControlType`](crate::core::CacheControlType)
/// enum with an `Other(String)` catch-all. The two modules represent different
/// layers (wire format vs. canonical internal representation) and duplicating
/// the struct keeps the serde concerns cleanly separated.
///
/// Anthropic models accept `cache_control` on messages; the proxy
/// preserves the annotation when translating between formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheControl {
    /// Discriminator – typically `"ephemeral"`.
    pub r#type: String,
}

impl CacheControl {
    /// The known cache-control type value used by Anthropic.
    pub const EPHEMERAL: &str = "ephemeral";
}

// ---------------------------------------------------------------------------
// Streaming options
// ---------------------------------------------------------------------------

/// Controls extra metadata returned alongside streaming chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamOptions {
    /// When `true`, the final chunk includes a [`UsageInfo`] object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// A tool (function) that the model may invoke during generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    /// Always `"function"` for function-calling tools.
    ///
    /// Kept as a bare `String` for serde round-tripping with providers that may
    /// introduce new tool types in the future. The adapter does not validate this
    /// value; callers that need to enforce `"function"` should check at decode time.
    pub r#type: String,
    /// Schema of the function the model can call.
    pub function: FunctionDef,
}

/// Describes a callable function, including its JSON-schema parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Position inside the `tool_calls` array (streaming deltas only).
    ///
    /// Uses `i32` to match the OpenAI wire format (JSON integers). The OpenAI
    /// spec defines this as a non-negative integer; negative values are
    /// semantically meaningless. The stream encoder clamps `usize` core indices
    /// to `i32::MAX` when converting. See `clamp_tool_index` in the adapter.
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
///
/// Both fields are `Option<String>` to accommodate streaming deltas where the
/// server sends partial `ToolCall`s that the client merges by `index`. For
/// non-streaming (complete) responses, callers should validate that `name` and
/// `arguments` are present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"`, `"assistant"`, or `"tool"`.
    ///
    /// Defaults to empty when absent (streaming deltas often omit this).
    /// Validation of known role values happens at decode time in the adapter
    /// rather than at the type level, to tolerate provider-specific extensions.
    #[serde(default)]
    pub role: String,
    /// The content body of the message.
    ///
    /// OpenAI allows `content` to be either a plain string or an array of
    /// structured content parts (e.g. `[{"type":"text","text":"..."},
    /// {"type":"image_url","image_url":{...}}]`).  Using `serde_json::Value`
    /// lets us accept both forms without rejecting multimodal requests.
    ///
    /// Defaults to `Value::String("")` when absent (streaming deltas often
    /// omit this).
    #[serde(default = "default_content")]
    pub content: serde_json::Value,
    /// Chain-of-thought text emitted by reasoning models.
    ///
    /// Some OpenAI-compatible providers (e.g. DeepSeek) use the `reasoning`
    /// field name instead of `reasoning_content`. The `serde` alias ensures
    /// both names deserialize into this field.
    #[serde(alias = "reasoning", skip_serializing_if = "Option::is_none")]
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
    /// Model refusal text (OpenAI-specific: present on assistant messages when
    /// the model refuses to answer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// Tool-result error flag (only meaningful when `role == "tool"`).
    /// Some OpenAI-compatible providers use `status` instead of `is_error`.
    #[serde(default, alias = "status", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ChatMessage {
    /// Extract the text content as a `String`.
    ///
    /// Handles both forms accepted by the OpenAI Chat Completions API:
    /// - `Value::String(s)` — returned directly.
    /// - `Value::Array(parts)` — concatenates all `"type":"text"` parts' `"text"`
    ///   fields. Non-text parts (e.g. `image_url`) are silently skipped.
    /// - `Value::Null` — returns an empty string.
    ///
    /// Any other value type returns an empty string with a warning.
    pub fn content_text(&self) -> String {
        match &self.content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(parts) => {
                let mut out = String::new();
                for part in parts {
                    if let serde_json::Value::Object(map) = part {
                        if map.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                                out.push_str(text);
                            }
                        }
                    }
                }
                out
            }
            serde_json::Value::Null => String::new(),
            other => {
                tracing::warn!(
                    ?other,
                    "ChatMessage::content_text: unexpected content type, returning empty string"
                );
                String::new()
            }
        }
    }

    /// Returns `true` when `content` is semantically empty.
    ///
    /// This is `true` when the value is `Null`, an empty string (`""`), or an
    /// empty array. It is the replacement for the old `msg.content.is_empty()`
    /// checks used throughout the codebase.
    pub fn content_is_empty(&self) -> bool {
        match &self.content {
            serde_json::Value::String(s) => s.is_empty(),
            serde_json::Value::Array(a) => a.is_empty(),
            serde_json::Value::Null => true,
            _ => false,
        }
    }
}

/// Default value for [`ChatMessage::content`]: an empty JSON string.
fn default_content() -> serde_json::Value {
    serde_json::Value::String(String::new())
}

// ---------------------------------------------------------------------------
// Chat completion request
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/chat/completions`.
///
/// This type serves a dual purpose: it is both deserialized from inbound client
/// requests (via `openai_chat::decode_request`) and constructed internally for
/// outbound provider calls (via `transform_request()`). Unknown fields from
/// clients are captured via the `extra` flattened map rather than silently
/// dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    ///
    /// OpenAI renamed `max_tokens` to `max_completion_tokens` in later API
    /// versions.  Both names are accepted via the serde alias; the canonical
    /// field name follows the newer convention.
    ///
    /// Uses `i32` for consistency with provider wire formats. Negative values
    /// are semantically invalid; the adapter should validate `max_tokens >= 0`
    /// during decode.
    #[serde(
        rename = "max_completion_tokens",
        alias = "max_tokens",
        skip_serializing_if = "Option::is_none"
    )]
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
    ///
    /// Kept as `Option<serde_json::Value>` (rather than `Option<Vec<String>>`)
    /// to preserve the raw OpenAI wire format for passthrough. The decode
    /// function in `openai_chat::decode_request` normalises this into
    /// `SamplingOptions.stop` using the same logic as `core::deserialize_stop`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<serde_json::Value>,
    /// Extra streaming metadata flags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// End-user identifier for abuse monitoring.
    ///
    /// Maps to `RequestMetadata.user_id` during decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Catch-all for unrecognized client fields, preserved in
    /// `RequestMetadata.raw` during decode so nothing is silently dropped.
    ///
    /// Because `#[serde(flatten)]` and `#[serde(deny_unknown_fields)]` are
    /// mutually exclusive, this struct cannot use `deny_unknown_fields`.
    /// Fields that look like typos of known fields (e.g. `"temperatur"` instead
    /// of `"temperature"`) will end up here silently. The decode function
    /// should warn on such cases if desired.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
///
/// `Default` is derived so the [`ChatCompletionResponse`] `usage` field can use
/// `#[serde(default)]`: some OpenAI-compatible providers omit `usage` entirely
/// on 2xx responses, and absent usage should yield zero counts rather than a
/// decode failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UsageInfo {
    /// Tokens consumed by the prompt.
    pub prompt_tokens: i32,
    /// Tokens produced by the completion.
    pub completion_tokens: i32,
    /// `prompt_tokens + completion_tokens`.
    ///
    /// The proxy recomputes this via `saturating_add` during encoding rather than
    /// passing through the provider's value, ensuring consistency. If a provider
    /// reports a different total, the proxy's computed value takes precedence.
    pub total_tokens: i32,
    /// Prompt tokens served from a cache (Anthropic / provider-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<i32>,
    /// Prompt tokens that missed the cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<i32>,
}

/// Deserialize `T`, treating a JSON `null` as `T::default()`. Combined with
/// `#[serde(default)]` (which covers an absent field), this lets the
/// [`ChatCompletionResponse`] `usage` field decode cleanly whether the provider
/// omits it or sends `null` — both yield zero counts instead of a 502.
fn deserialize_default_on_null<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    use serde::Deserialize as _;
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Choice
// ---------------------------------------------------------------------------

/// A single completion alternative inside a response or chunk.
///
/// The `message` and `delta` fields are mutually exclusive in practice:
/// non-streaming responses use `message`, streaming chunks use `delta`.
/// This is not enforced at the type level to keep the struct compatible with
/// both streaming and non-streaming JSON shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    ///
    /// Some OpenAI-compatible providers (e.g. certain OpenRouter backends) omit
    /// `usage` entirely or send it as `null` on 2xx responses. The
    /// `#[serde(default)]` + `deserialize_default_on_null` combination maps both
    /// cases to zero token counts so the response decodes normally instead of
    /// failing into a 502 "provider response decode error".
    #[serde(default, deserialize_with = "deserialize_default_on_null")]
    pub usage: UsageInfo,
    /// Catch-all for provider-specific response fields (e.g. `service_tier`,
    /// `system_fingerprint`) that the proxy does not model explicitly.
    ///
    /// Because this struct uses `#[serde(flatten)]` here, `deny_unknown_fields`
    /// cannot also be applied. Unknown fields are captured rather than rejected.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Nested error details.
    pub error: ErrorDetails,
}

/// Detailed error information inside an [`ErrorResponse`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control_round_trip() {
        let cc = CacheControl {
            r#type: "ephemeral".into(),
        };
        let json = serde_json::to_string(&cc).unwrap();
        assert_eq!(json, r#"{"type":"ephemeral"}"#);
        let back: CacheControl = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cc);
    }

    #[test]
    fn error_response_round_trip() {
        let err = ErrorResponse {
            error: ErrorDetails {
                r#type: Some("invalid_request_error".into()),
                message: "model is required".into(),
                code: Some("invalid_api_key".into()),
            },
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: ErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, err);
    }

    #[test]
    fn tool_def_round_trip() {
        let tool = ToolDef {
            r#type: "function".into(),
            function: FunctionDef {
                name: "get_weather".into(),
                description: Some("Get the weather".into()),
                parameters: Some(serde_json::json!({"type": "object"})),
            },
        };
        let json = serde_json::to_string(&tool).unwrap();
        let back: ToolDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tool);
    }

    #[test]
    fn chat_message_reasoning_alias() {
        // Verify that the "reasoning" alias deserializes into reasoning_content.
        let json = r#"{"role":"assistant","reasoning":"thinking..."}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.reasoning_content.as_deref(), Some("thinking..."));
    }

    #[test]
    fn usage_info_round_trip() {
        let usage = UsageInfo {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_cache_hit_tokens: Some(10),
            prompt_cache_miss_tokens: None,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let back: UsageInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }

    #[test]
    fn chat_completion_request_accepts_unknown_fields() {
        let json = r#"{"model":"gpt-4o","messages":[],"future_field":"value"}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.extra.get("future_field").unwrap().as_str(),
            Some("value")
        );
    }

    #[test]
    fn choice_round_trip() {
        let choice = Choice {
            index: 0,
            message: Some(ChatMessage {
                role: "assistant".into(),
                content: serde_json::Value::String("hello".into()),
                reasoning_content: None,
                tool_calls: vec![],
                name: None,
                tool_call_id: None,
                cache_control: None,
                refusal: None,
                is_error: None,
            }),
            finish_reason: Some("stop".into()),
            delta: None,
        };
        let json = serde_json::to_string(&choice).unwrap();
        let back: Choice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, choice);
    }
}
