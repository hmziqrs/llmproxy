//! Core protocol types for the LLM proxy.
//!
//! This module defines the normalized internal representation that sits between
//! client protocol adapters and provider adapters.  Every adapter translates
//! **to** or **from** these types -- never directly to another adapter's wire
//! format.
//!
//! ## Design rules
//!
//! * Chat-endpoint focused.  Do not add embeddings, image generation, audio
//!   endpoints, rerank, files, or batch families here.
//! * Chat content still preserves image/document/audio/video blocks because
//!   those can appear inside chat requests and responses.
//! * The stream error type is `CoreStreamError` (not `CoreError`) to avoid
//!   colliding with the config / registry error owned by the core crate.
//!
//! ## Opaque field safety
//!
//! Several fields hold opaque JSON data that passes through the proxy without
//! interpretation: [`RequestMetadata::raw`], [`ProviderHints::raw`],
//! [`CoreResponse::provider_meta`], [`SamplingOptions::thinking`], and
//! [`CoreToolChoice::Raw`].  These opaque fields derive `Debug` which prints
//! values verbatim.  Adapters **must not** store secrets (API keys, bearer
//! tokens, etc.) in any of these fields, as the derived `Debug` output will
//! print them in full plaintext to any log or error message.  Strip credentials
//! before placing data into these fields.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ModelRef
// ---------------------------------------------------------------------------

/// A model identifier that tracks both the client-facing name and the resolved
/// upstream name.
///
/// `requested` is always the model the client asked for.  Provider resolution
/// may populate `upstream` with the actual upstream model id.  Adapters must
/// **not** replace `requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    /// The model name as requested by the client.
    pub requested: String,
    /// The resolved upstream model name, if different from `requested`.
    pub upstream: Option<String>,
}

impl ModelRef {
    /// Returns the effective model identifier for the provider.
    ///
    /// If `upstream` is set it is used; otherwise `requested` is used.
    pub fn provider_model(&self) -> &str {
        self.upstream.as_deref().unwrap_or(&self.requested)
    }
}

// ---------------------------------------------------------------------------
// CoreRequest
// ---------------------------------------------------------------------------

/// A normalized chat request.
///
/// This is the canonical internal representation of a client's intent.
/// Client adapters decode their wire format into this shape; provider adapters
/// encode it into the provider's wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreRequest {
    /// Model identifier (client-facing + optional upstream override).
    pub model: ModelRef,
    /// System / developer instructions (canonical location).
    ///
    /// Client adapters should normalise system-role messages into this field
    /// where the client protocol allows it.
    pub system: Vec<CoreContent>,
    /// Conversation turns after system content has been separated.
    pub messages: Vec<CoreMessage>,
    /// Tool definitions available to the model.
    pub tools: Vec<CoreTool>,
    /// Controls which (if any) tool the model must call.
    pub tool_choice: Option<CoreToolChoice>,
    /// Sampling parameters (temperature, top-p, etc.).
    pub sampling: SamplingOptions,
    /// Whether the client requested streaming.
    pub stream: bool,
    /// Caller metadata (user id, arbitrary key-value pairs).
    pub metadata: RequestMetadata,
    /// Opaque hints consumed only by provider adapters.
    pub provider_hints: ProviderHints,
}

// ---------------------------------------------------------------------------
// CoreMessage / CoreRole
// ---------------------------------------------------------------------------

/// A single conversation turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreMessage {
    /// Speaker role.
    pub role: CoreRole,
    /// Content blocks in this turn.
    pub content: Vec<CoreContent>,
}

/// Speaker role within a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreRole {
    /// System-level instructions (only for protocols that cannot separate
    /// system content cleanly; prefer `CoreRequest.system`).
    System,
    /// End-user message.
    User,
    /// Model response.
    Assistant,
    /// Tool result.
    Tool,
}

// ---------------------------------------------------------------------------
// CoreContent
// ---------------------------------------------------------------------------

/// A single content block inside a message or system prompt.
///
/// Variants cover text, media, tool interactions, and extended thinking.
/// Each variant is a struct-like enum arm so that fields are named and
/// self-documenting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CoreContent {
    /// Plain text.
    Text {
        /// The text content.
        text: String,
        /// Optional cache-control directive.
        cache: Option<CacheControl>,
    },
    /// Image content (source is provider-specific JSON).
    Image {
        /// Opaque image source data.
        source: serde_json::Value,
    },
    /// Document / file attachment.
    Document {
        /// Opaque document source data.
        source: serde_json::Value,
    },
    /// Audio content.
    Audio {
        /// Opaque audio source data.
        source: serde_json::Value,
    },
    /// Video content.
    Video {
        /// Opaque video source data.
        source: serde_json::Value,
    },
    /// A tool invocation emitted by the model.
    ToolUse {
        /// Unique tool-call identifier.
        id: String,
        /// Name of the tool being called.
        name: String,
        /// JSON arguments for the tool call.
        input: serde_json::Value,
    },
    /// The result of a tool invocation, fed back to the model.
    ToolResult {
        /// The tool-call id this result corresponds to.
        tool_use_id: String,
        /// Content of the result (can nest text, images, etc.).
        content: Vec<CoreContent>,
        /// Whether this result represents an error.
        is_error: bool,
    },
    /// Extended thinking content.
    Thinking {
        /// The thinking text.
        text: String,
        /// Optional cryptographic signature (e.g. Anthropic-style).
        ///
        /// This field contains security-sensitive data.  It should not appear
        /// in production logs verbatim, as derived `Debug` output will print
        /// it in full.
        signature: Option<String>,
    },
    /// Redacted thinking block whose plaintext is not available.
    RedactedThinking {
        /// Opaque redacted data.
        data: serde_json::Value,
    },
    /// A model refusal to answer.
    Refusal {
        /// Human-readable refusal text.
        text: String,
    },
}

// ---------------------------------------------------------------------------
// CacheControl
// ---------------------------------------------------------------------------

/// Cache-control directive attached to a content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheControl {
    /// Discriminator -- typically `"ephemeral"`.
    pub r#type: CacheControlType,
}

/// Known cache control type variants.
///
/// Uses an enum with a catch-all `Other` variant so that unknown values from
/// future provider extensions are preserved rather than rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheControlType {
    /// Anthropic-style ephemeral cache control.
    Ephemeral,
    /// An unknown cache control type, preserved as a raw string.
    Other(String),
}

impl From<String> for CacheControlType {
    fn from(value: String) -> Self {
        match value.as_str() {
            "ephemeral" => Self::Ephemeral,
            _ => Self::Other(value),
        }
    }
}

impl From<CacheControlType> for String {
    fn from(val: CacheControlType) -> Self {
        match val {
            CacheControlType::Ephemeral => "ephemeral".to_owned(),
            CacheControlType::Other(s) => s,
        }
    }
}

impl Serialize for CacheControlType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s: String = self.clone().into();
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for CacheControlType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(s.into())
    }
}

// ---------------------------------------------------------------------------
// CoreTool / CoreToolChoice
// ---------------------------------------------------------------------------

/// A tool definition available to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreTool {
    /// Name the model uses to reference this tool.
    pub name: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// JSON Schema describing the tool's parameters.
    ///
    /// Adapters are responsible for validating the schema shape when
    /// constructing a `CoreTool`, since the type system cannot enforce that
    /// this `Value` is a valid JSON Schema object.
    pub input_schema: serde_json::Value,
}

/// Controls which (if any) tool the model must call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CoreToolChoice {
    /// The model decides whether to call a tool.
    Auto,
    /// The model must call **some** tool.
    Any,
    /// The model must not call any tool.
    None,
    /// The model must call the named tool.
    Tool {
        /// Name of the tool to call.
        name: String,
    },
    /// Temporary preservation of a client tool-choice shape that has no
    /// canonical variant yet.
    ///
    /// Must not contain unrelated wire nesting.  Adapter tests must either
    /// round-trip it intentionally or move it into metadata / provider hints.
    Raw(serde_json::Value),
}

// ---------------------------------------------------------------------------
// SamplingOptions
// ---------------------------------------------------------------------------

/// Sampling parameters that control generation behaviour.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingOptions {
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Nucleus sampling parameter.
    pub top_p: Option<f64>,
    /// Maximum number of tokens to generate.
    pub max_tokens: Option<i32>,
    /// Stop sequences.  Typed as `Vec<String>` matching OpenAI/Anthropic wire
    /// formats where stop sequences are always a list of strings.
    pub stop: Option<Vec<String>>,
    /// Reasoning effort level (e.g. `"low"`, `"medium"`, `"high"`).
    pub reasoning_effort: Option<String>,
    /// Extended thinking configuration (provider-specific JSON).
    ///
    /// This is intentionally opaque.  Expected shapes vary by provider, e.g.
    /// Anthropic: `{"type": "enabled", "budget_tokens": N}`.
    pub thinking: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// RequestMetadata
// ---------------------------------------------------------------------------

/// Caller metadata attached to a request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestMetadata {
    /// Optional user identifier.
    pub user_id: Option<String>,
    /// Arbitrary key-value pairs for opaque data.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// ProviderHints
// ---------------------------------------------------------------------------

/// Opaque hints consumed only by provider adapters.
///
/// No adapter may special-case another protocol's hints.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderHints {
    /// Arbitrary key-value pairs.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// CoreResponse
// ---------------------------------------------------------------------------

/// A normalized non-streaming chat response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreResponse {
    /// Response identifier (echoed from the provider when available).
    pub id: Option<String>,
    /// Model identifier for this response.
    pub model: ModelRef,
    /// Content blocks in the response.
    pub content: Vec<CoreContent>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// The stop sequence that caused generation to stop, if any.
    pub stop_sequence: Option<String>,
    /// Token usage statistics.
    pub usage: Usage,
    /// Provider-specific metadata (warnings, extra fields, etc.).
    ///
    /// Uses a `Map` (not bare `Value`) so that the field serializes as `{}`
    /// when empty rather than `null`, distinguishing "no metadata" from
    /// "null metadata".
    #[serde(default)]
    pub provider_meta: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// StopReason
// ---------------------------------------------------------------------------

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The model finished its turn naturally.
    EndTurn,
    /// The maximum token count was reached.
    MaxTokens,
    /// The model stopped to invoke a tool.
    ToolUse,
    /// A stop sequence was matched.
    StopSequence,
    /// The model refused to answer.
    Refusal,
    /// An error occurred.
    Error,
    /// The stop reason is not recognised.
    Unknown,
}

// ---------------------------------------------------------------------------
// Usage / UsageProvenance
// ---------------------------------------------------------------------------

/// Token usage statistics.
///
/// Must carry provenance.  Never report fabricated or zero-filled usage as
/// [`UsageProvenance::ProviderReported`]; absent or zero-filled provider
/// usage is [`UsageProvenance::SyntheticZero`] / [`UsageProvenance::Unknown`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Number of tokens in the prompt.
    pub input_tokens: i32,
    /// Number of tokens in the completion.
    pub output_tokens: i32,
    /// Reasoning token count (OpenAI/Responses-style).
    pub reasoning_tokens: Option<i32>,
    /// Tokens written to the cache during this request.
    pub cache_creation_input_tokens: Option<i32>,
    /// Tokens read from the cache during this request.
    pub cache_read_input_tokens: Option<i32>,
    /// Where the usage numbers came from.
    pub provenance: UsageProvenance,
}

/// Provenance of token usage numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UsageProvenance {
    /// The provider reported these numbers.
    ProviderReported,
    /// The proxy synthesised zero values because the provider did not report.
    SyntheticZero,
    /// Provenance is unknown.
    #[default]
    Unknown,
}

// ---------------------------------------------------------------------------
// CoreEvent (streaming)
// ---------------------------------------------------------------------------

/// A single normalised streaming event.
///
/// V1 streaming carries only `Text`, `Thinking`, and `ToolUse` incrementally
/// (text/thinking/tool-call deltas).  `Image`, `Document`, `Audio`, and
/// `Video` have no per-delta stream event; if a provider streams them the
/// adapter buffers the block and emits it through the non-stream
/// [`CoreResponse`] content path, or rejects it.  Stream encoders therefore
/// never receive deltas for those kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CoreEvent {
    /// The response has started.
    MessageStart {
        /// Response identifier.
        id: Option<String>,
        /// Model being used.
        model: ModelRef,
    },
    /// A new content block has started.
    ContentStart {
        /// Index of the content block.
        index: usize,
        /// Kind of content block.
        kind: ContentKind,
    },
    /// Incremental text content.
    TextDelta {
        /// Index of the content block.
        index: usize,
        /// The text delta.
        text: String,
    },
    /// Incremental thinking content.
    ThinkingDelta {
        /// Index of the content block.
        index: usize,
        /// The thinking text delta.
        text: String,
    },
    /// A tool call has started.
    ToolCallStart {
        /// Index of the content block.
        index: usize,
        /// Unique tool-call identifier.
        id: String,
        /// Name of the tool being called.
        name: String,
    },
    /// Incremental tool-call arguments.
    ToolCallDelta {
        /// Index of the content block.
        index: usize,
        /// Delta of the JSON arguments string.
        args_delta: String,
    },
    /// A tool call has finished.
    ToolCallStop {
        /// Index of the content block.
        index: usize,
    },
    /// Usage information update.
    UsageDelta {
        /// Token usage so far.
        usage: Usage,
    },
    /// The response has finished.
    MessageStop {
        /// Why the model stopped generating.
        stop_reason: StopReason,
        /// The stop sequence that caused generation to stop, if any.
        stop_sequence: Option<String>,
    },
    /// An error occurred during streaming.
    Error {
        /// The stream error details.
        error: CoreStreamError,
    },
    /// Keep-alive / heartbeat.
    Ping,
}

// ---------------------------------------------------------------------------
// ContentKind
// ---------------------------------------------------------------------------

/// Discriminator for content block types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentKind {
    /// Plain text.
    Text,
    /// Extended thinking.
    Thinking,
    /// Tool invocation.
    ToolUse,
    /// Tool result.
    ToolResult,
    /// Image.
    Image,
    /// Document / file.
    Document,
    /// Audio.
    Audio,
    /// Video.
    Video,
    /// Model refusal.
    Refusal,
}

// ---------------------------------------------------------------------------
// CoreStreamError / CoreStreamErrorKind
// ---------------------------------------------------------------------------

/// An error that occurred during streaming.
///
/// Named `CoreStreamError` (not `CoreError`) to avoid colliding with the
/// config/registry error owned by the core crate.  The two are unrelated
/// types in different crates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreStreamError {
    /// Category of the error.
    pub kind: CoreStreamErrorKind,
    /// Human-readable error message.
    pub message: String,
}

/// Category of a stream error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreStreamErrorKind {
    /// The request was malformed or invalid.
    InvalidRequest,
    /// Authentication failed.
    Authentication,
    /// The caller lacks permission.
    Permission,
    /// A rate limit was exceeded.
    RateLimit,
    /// The upstream provider returned an error.
    Upstream,
    /// An internal proxy error.
    Internal,
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ref_provider_model_uses_requested_without_override() {
        let mr = ModelRef {
            requested: "gpt-4o".into(),
            upstream: None,
        };
        assert_eq!(mr.provider_model(), "gpt-4o");
    }

    #[test]
    fn model_ref_provider_model_uses_upstream_override() {
        let mr = ModelRef {
            requested: "my-alias".into(),
            upstream: Some("gpt-4o-2024-08-06".into()),
        };
        assert_eq!(mr.provider_model(), "gpt-4o-2024-08-06");
    }

    #[test]
    fn sampling_options_default_has_no_overrides() {
        let opts = SamplingOptions::default();
        assert!(opts.temperature.is_none());
        assert!(opts.top_p.is_none());
        assert!(opts.max_tokens.is_none());
        assert!(opts.stop.is_none());
        assert!(opts.reasoning_effort.is_none());
        assert!(opts.thinking.is_none());
    }

    #[test]
    fn usage_default_is_zero() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert!(usage.reasoning_tokens.is_none());
        assert!(usage.cache_creation_input_tokens.is_none());
        assert!(usage.cache_read_input_tokens.is_none());
    }

    #[test]
    fn core_content_tool_result_can_nest_text() {
        let result = CoreContent::ToolResult {
            tool_use_id: "call_123".into(),
            content: vec![
                CoreContent::Text {
                    text: "it worked".into(),
                    cache: None,
                },
            ],
            is_error: false,
        };
        match result {
            CoreContent::ToolResult {
                content,
                is_error,
                tool_use_id,
            } => {
                assert!(!is_error);
                assert_eq!(tool_use_id, "call_123");
                assert_eq!(content.len(), 1);
                if let CoreContent::Text { text, .. } = &content[0] {
                    assert_eq!(text, "it worked");
                } else {
                    panic!("expected Text variant");
                }
            }
            _ => panic!("expected ToolResult variant"),
        }
    }

    #[test]
    fn core_response_can_preserve_stop_sequence() {
        let resp = CoreResponse {
            id: Some("resp_abc".into()),
            model: ModelRef {
                requested: "claude-sonnet-4-20250514".into(),
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
        assert_eq!(resp.stop_reason, StopReason::StopSequence);
        assert_eq!(resp.stop_sequence.as_deref(), Some("\n"));
    }

    #[test]
    fn message_stop_event_can_preserve_stop_sequence() {
        let event = CoreEvent::MessageStop {
            stop_reason: StopReason::StopSequence,
            stop_sequence: Some("END".into()),
        };
        match event {
            CoreEvent::MessageStop {
                stop_reason,
                stop_sequence,
            } => {
                assert_eq!(stop_reason, StopReason::StopSequence);
                assert_eq!(stop_sequence.as_deref(), Some("END"));
            }
            _ => panic!("expected MessageStop variant"),
        }
    }

    #[test]
    fn core_request_preserves_client_intent() {
        let req = CoreRequest {
            model: ModelRef {
                requested: "my-model".into(),
                upstream: None,
            },
            system: vec![CoreContent::Text {
                text: "you are helpful".into(),
                cache: None,
            }],
            messages: vec![CoreMessage {
                role: CoreRole::User,
                content: vec![CoreContent::Text {
                    text: "hello".into(),
                    cache: None,
                }],
            }],
            tools: vec![CoreTool {
                name: "get_weather".into(),
                description: Some("Get weather".into()),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            }],
            tool_choice: Some(CoreToolChoice::Auto),
            sampling: SamplingOptions {
                temperature: Some(0.7),
                ..Default::default()
            },
            stream: false,
            metadata: RequestMetadata::default(),
            provider_hints: ProviderHints::default(),
        };
        assert_eq!(req.model.requested, "my-model");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.tools.len(), 1);
        assert!(!req.stream);
    }

    #[test]
    fn core_request_preserves_metadata_and_provider_hints() {
        let mut raw_meta = serde_json::Map::new();
        raw_meta.insert(
            "trace_id".into(),
            serde_json::Value::String("abc-123".into()),
        );
        let mut raw_hints = serde_json::Map::new();
        raw_hints.insert(
            "prefer_region".into(),
            serde_json::Value::String("us-east".into()),
        );

        let req = CoreRequest {
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions::default(),
            stream: false,
            metadata: RequestMetadata {
                user_id: Some("user-42".into()),
                raw: raw_meta,
            },
            provider_hints: ProviderHints { raw: raw_hints },
        };
        assert_eq!(req.metadata.user_id.as_deref(), Some("user-42"));
        assert_eq!(
            req.metadata.raw.get("trace_id").unwrap().as_str(),
            Some("abc-123")
        );
        assert_eq!(
            req.provider_hints
                .raw
                .get("prefer_region")
                .unwrap()
                .as_str(),
            Some("us-east")
        );
    }

    #[test]
    fn core_request_system_field_is_canonical() {
        let system = vec![
            CoreContent::Text {
                text: "system prompt".into(),
                cache: None,
            },
            CoreContent::Text {
                text: "extra instruction".into(),
                cache: Some(CacheControl {
                    r#type: CacheControlType::Ephemeral,
                }),
            },
        ];
        let req = CoreRequest {
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            system,
            messages: vec![CoreMessage {
                role: CoreRole::User,
                content: vec![CoreContent::Text {
                    text: "hi".into(),
                    cache: None,
                }],
            }],
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions::default(),
            stream: false,
            metadata: RequestMetadata::default(),
            provider_hints: ProviderHints::default(),
        };
        assert_eq!(req.system.len(), 2);
        // System content stays in `system`, not in messages.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, CoreRole::User);
    }

    #[test]
    fn core_request_round_trips_through_json() {
        let req = CoreRequest {
            model: ModelRef {
                requested: "model-a".into(),
                upstream: Some("model-b".into()),
            },
            system: vec![CoreContent::Text {
                text: "sys".into(),
                cache: None,
            }],
            messages: vec![CoreMessage {
                role: CoreRole::User,
                content: vec![CoreContent::Text {
                    text: "hi".into(),
                    cache: None,
                }],
            }],
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions {
                temperature: Some(0.5),
                max_tokens: Some(100),
                ..Default::default()
            },
            stream: true,
            metadata: RequestMetadata::default(),
            provider_hints: ProviderHints::default(),
        };
        let json = serde_json::to_string(&req).expect("serialize CoreRequest");
        let back: CoreRequest = serde_json::from_str(&json).expect("deserialize CoreRequest");
        // With PartialEq on CoreRequest, we can compare the whole struct.
        assert_eq!(req, back);
    }

    #[test]
    fn core_response_round_trips_through_json() {
        let resp = CoreResponse {
            id: Some("resp_1".into()),
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![
                CoreContent::Text {
                    text: "hello".into(),
                    cache: None,
                },
                CoreContent::ToolUse {
                    id: "call_1".into(),
                    name: "fn".into(),
                    input: serde_json::json!({"x": 1}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                reasoning_tokens: Some(5),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(3),
                provenance: UsageProvenance::ProviderReported,
            },
            provider_meta: serde_json::json!({"warnings": []}).as_object().unwrap().clone(),
        };
        let json = serde_json::to_string(&resp).expect("serialize CoreResponse");
        let back: CoreResponse = serde_json::from_str(&json).expect("deserialize CoreResponse");
        assert_eq!(back.id.as_deref(), Some("resp_1"));
        assert_eq!(back.stop_reason, StopReason::ToolUse);
        assert_eq!(back.usage.input_tokens, 10);
        assert_eq!(back.usage.output_tokens, 20);
        assert_eq!(back.usage.reasoning_tokens, Some(5));
        assert_eq!(back.usage.provenance, UsageProvenance::ProviderReported);
    }

    #[test]
    fn core_event_round_trips_through_json() {
        let events: Vec<CoreEvent> = vec![
            CoreEvent::MessageStart {
                id: Some("msg_1".into()),
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
            CoreEvent::TextDelta {
                index: 0,
                text: "hello ".into(),
            },
            CoreEvent::TextDelta {
                index: 0,
                text: "world".into(),
            },
            CoreEvent::ThinkingDelta {
                index: 1,
                text: "let me think".into(),
            },
            CoreEvent::ToolCallStart {
                index: 2,
                id: "call_1".into(),
                name: "get_weather".into(),
            },
            CoreEvent::ToolCallDelta {
                index: 2,
                args_delta: "{\"ci".into(),
            },
            CoreEvent::ToolCallDelta {
                index: 2,
                args_delta: "ty\": \"SF\"}".into(),
            },
            CoreEvent::ToolCallStop { index: 2 },
            CoreEvent::UsageDelta {
                usage: Usage {
                    input_tokens: 50,
                    output_tokens: 100,
                    reasoning_tokens: None,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                    provenance: UsageProvenance::ProviderReported,
                },
            },
            CoreEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                stop_sequence: None,
            },
            CoreEvent::Ping,
            CoreEvent::Error {
                error: CoreStreamError {
                    kind: CoreStreamErrorKind::RateLimit,
                    message: "too many requests".into(),
                },
            },
        ];

        let json = serde_json::to_string(&events).expect("serialize CoreEvent vec");
        let back: Vec<CoreEvent> = serde_json::from_str(&json).expect("deserialize CoreEvent vec");
        // With PartialEq on CoreEvent, we can compare the full vectors.
        assert_eq!(events, back);
    }

    #[test]
    fn core_tool_choice_raw_round_trips_when_intentional() {
        let raw_val = serde_json::json!({"type": "some_custom", "mode": "strict"});
        let choice = CoreToolChoice::Raw(raw_val.clone());
        let json = serde_json::to_string(&choice).expect("serialize CoreToolChoice");
        let back: CoreToolChoice = serde_json::from_str(&json).expect("deserialize CoreToolChoice");
        match back {
            CoreToolChoice::Raw(v) => assert_eq!(v, raw_val),
            _ => panic!("expected Raw variant"),
        }
    }

    #[test]
    fn usage_default_provenance_is_unknown() {
        let usage = Usage::default();
        assert_eq!(usage.provenance, UsageProvenance::Unknown);
    }

    #[test]
    fn usage_preserves_reasoning_tokens() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 200,
            reasoning_tokens: Some(42),
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(20),
            provenance: UsageProvenance::ProviderReported,
        };
        let json = serde_json::to_string(&usage).expect("serialize Usage");
        let back: Usage = serde_json::from_str(&json).expect("deserialize Usage");
        assert_eq!(back.reasoning_tokens, Some(42));
        assert_eq!(back.cache_creation_input_tokens, Some(10));
        assert_eq!(back.cache_read_input_tokens, Some(20));
        assert_eq!(back.provenance, UsageProvenance::ProviderReported);
    }

    // =======================================================================
    // New edge-case and coverage tests (audit round 1)
    // =======================================================================

    #[test]
    fn cache_control_type_known_and_unknown_variants() {
        let ephemeral = CacheControlType::Ephemeral;
        assert_eq!(String::from(ephemeral.clone()), "ephemeral");
        let json = serde_json::to_string(&ephemeral).unwrap();
        assert_eq!(json, "\"ephemeral\"");
        let back: CacheControlType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CacheControlType::Ephemeral);

        let other = CacheControlType::Other("future_type".to_owned());
        assert_eq!(String::from(other.clone()), "future_type");
        let json = serde_json::to_string(&other).unwrap();
        let back: CacheControlType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CacheControlType::Other("future_type".to_owned()));
    }

    #[test]
    fn model_ref_equality_semantics() {
        let a = ModelRef {
            requested: "gpt-4o".into(),
            upstream: Some("gpt-4o-2024-08-06".into()),
        };
        let b = ModelRef {
            requested: "gpt-4o".into(),
            upstream: Some("gpt-4o-2024-08-06".into()),
        };
        assert_eq!(a, b);

        let c = ModelRef {
            requested: "gpt-4o".into(),
            upstream: None,
        };
        assert_ne!(a, c);

        let d = ModelRef {
            requested: "claude".into(),
            upstream: Some("gpt-4o-2024-08-06".into()),
        };
        assert_ne!(a, d);
    }

    #[test]
    fn core_content_handles_empty_strings() {
        let variants = vec![
            CoreContent::Text {
                text: String::new(),
                cache: None,
            },
            CoreContent::ToolUse {
                id: String::new(),
                name: String::new(),
                input: serde_json::Value::Null,
            },
            CoreContent::ToolResult {
                tool_use_id: String::new(),
                content: vec![],
                is_error: false,
            },
            CoreContent::Thinking {
                text: String::new(),
                signature: Some(String::new()),
            },
            CoreContent::Refusal {
                text: String::new(),
            },
        ];
        for content in &variants {
            let json = serde_json::to_string(content).expect("serialize");
            let back: CoreContent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*content, back);
        }
    }

    #[test]
    fn core_request_rejects_malformed_json() {
        let cases = vec![
            "not json at all",
            "{",
            r#"{"model": 123}"#, // model should be object, not number
        ];
        for bad in cases {
            assert!(
                serde_json::from_str::<CoreRequest>(bad).is_err(),
                "expected error for: {bad}"
            );
        }
    }

    #[test]
    fn core_response_rejects_malformed_json() {
        let cases = vec![
            "not json at all",
            "{",
            r#"{"id": 123}"#, // id should be string or null
        ];
        for bad in cases {
            assert!(
                serde_json::from_str::<CoreResponse>(bad).is_err(),
                "expected error for: {bad}"
            );
        }
    }

    #[test]
    fn core_event_rejects_malformed_json() {
        let cases = vec![
            "not json at all",
            "[",
            r#"{"TextDelta": "wrong"}"#,
        ];
        for bad in cases {
            assert!(
                serde_json::from_str::<CoreEvent>(bad).is_err(),
                "expected error for: {bad}"
            );
        }
    }

    #[test]
    fn sampling_options_handles_boundary_values() {
        let opts = SamplingOptions {
            temperature: Some(0.0),
            top_p: Some(0.0),
            max_tokens: Some(0),
            stop: None,
            reasoning_effort: None,
            thinking: None,
        };
        let json = serde_json::to_string(&opts).unwrap();
        let back: SamplingOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.temperature, Some(0.0));
        assert_eq!(back.max_tokens, Some(0));

        let opts_max = SamplingOptions {
            max_tokens: Some(i32::MAX),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts_max).unwrap();
        let back: SamplingOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_tokens, Some(i32::MAX));
    }

    #[test]
    fn usage_handles_i32_max() {
        let usage = Usage {
            input_tokens: i32::MAX,
            output_tokens: i32::MAX,
            reasoning_tokens: Some(i32::MAX),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            provenance: UsageProvenance::ProviderReported,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }

    #[test]
    fn core_response_handles_missing_optional_fields() {
        let json = r#"{
            "model": {"requested": "m"},
            "content": [],
            "stop_reason": "EndTurn",
            "usage": {"input_tokens": 0, "output_tokens": 0, "provenance": "Unknown"}
        }"#;
        let resp: CoreResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.id, None);
        assert!(resp.model.upstream.is_none());
        assert_eq!(resp.stop_sequence, None);
        assert_eq!(resp.usage.input_tokens, 0);
        assert_eq!(resp.usage.provenance, UsageProvenance::Unknown);
        assert!(resp.provider_meta.is_empty());
    }

    #[test]
    fn core_request_rejects_wrong_field_types() {
        let cases = vec![
            // model.requested should be string, not number
            r#"{"model": {"requested": 123}, "messages": [], "system": [], "tools": [], "sampling": {}, "stream": false, "metadata": {}, "provider_hints": {}}"#,
            // stream should be bool, not string
            r#"{"model": {"requested": "m"}, "messages": [], "system": [], "tools": [], "sampling": {}, "stream": "yes", "metadata": {}, "provider_hints": {}}"#,
        ];
        for bad in cases {
            assert!(
                serde_json::from_str::<CoreRequest>(bad).is_err(),
                "expected error for: {bad}"
            );
        }
    }

    #[test]
    fn core_request_handles_empty_collections() {
        let req = CoreRequest {
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions::default(),
            stream: false,
            metadata: RequestMetadata::default(),
            provider_hints: ProviderHints::default(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CoreRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
        assert!(back.system.is_empty());
        assert!(back.messages.is_empty());
        assert!(back.tools.is_empty());
    }

    #[test]
    fn tool_result_with_is_error_true_round_trips() {
        let result = CoreContent::ToolResult {
            tool_use_id: "call_err".into(),
            content: vec![CoreContent::Text {
                text: "something went wrong".into(),
                cache: None,
            }],
            is_error: true,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: CoreContent = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
    }

    #[test]
    fn tool_result_with_empty_content_round_trips() {
        let result = CoreContent::ToolResult {
            tool_use_id: "call_empty".into(),
            content: vec![],
            is_error: true,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: CoreContent = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
    }

    #[test]
    fn nested_tool_result_round_trips() {
        let inner = CoreContent::ToolResult {
            tool_use_id: "inner".into(),
            content: vec![CoreContent::Text {
                text: "nested".into(),
                cache: None,
            }],
            is_error: false,
        };
        let outer = CoreContent::ToolResult {
            tool_use_id: "outer".into(),
            content: vec![inner],
            is_error: false,
        };
        let json = serde_json::to_string(&outer).unwrap();
        let back: CoreContent = serde_json::from_str(&json).unwrap();
        assert_eq!(outer, back);
    }

    #[test]
    fn all_core_content_variants_round_trip() {
        let contents: Vec<CoreContent> = vec![
            CoreContent::Text {
                text: "hello".into(),
                cache: None,
            },
            CoreContent::Image {
                source: serde_json::json!({"url": "https://example.com/img.png"}),
            },
            CoreContent::Document {
                source: serde_json::json!({"url": "https://example.com/doc.pdf"}),
            },
            CoreContent::Audio {
                source: serde_json::json!({"data": "base64..."}),
            },
            CoreContent::Video {
                source: serde_json::json!({"url": "https://example.com/vid.mp4"}),
            },
            CoreContent::ToolUse {
                id: "call_1".into(),
                name: "fn".into(),
                input: serde_json::json!({"x": 1}),
            },
            CoreContent::ToolResult {
                tool_use_id: "call_1".into(),
                content: vec![CoreContent::Text {
                    text: "result".into(),
                    cache: None,
                }],
                is_error: false,
            },
            CoreContent::Thinking {
                text: "hmm".into(),
                signature: Some("sig123".into()),
            },
            CoreContent::RedactedThinking {
                data: serde_json::json!({"redacted": true}),
            },
            CoreContent::Refusal {
                text: "I cannot".into(),
            },
        ];
        for content in &contents {
            let json = serde_json::to_string(content).unwrap();
            let back: CoreContent = serde_json::from_str(&json).unwrap();
            assert_eq!(*content, back, "round-trip failed for variant");
        }
    }

    #[test]
    fn all_stop_reason_variants_round_trip() {
        let variants = vec![
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::ToolUse,
            StopReason::StopSequence,
            StopReason::Refusal,
            StopReason::Error,
            StopReason::Unknown,
        ];
        for variant in &variants {
            let json = serde_json::to_string(&variant).unwrap();
            let back: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back);
        }
    }

    #[test]
    fn all_core_stream_error_kind_variants_round_trip() {
        let variants = vec![
            CoreStreamErrorKind::InvalidRequest,
            CoreStreamErrorKind::Authentication,
            CoreStreamErrorKind::Permission,
            CoreStreamErrorKind::RateLimit,
            CoreStreamErrorKind::Upstream,
            CoreStreamErrorKind::Internal,
        ];
        for kind in &variants {
            let err = CoreStreamError {
                kind: *kind,
                message: format!("test error for {kind:?}"),
            };
            let json = serde_json::to_string(&err).unwrap();
            let back: CoreStreamError = serde_json::from_str(&json).unwrap();
            assert_eq!(err, back);
        }
    }

    #[test]
    fn all_core_tool_choice_variants_round_trip() {
        let variants = vec![
            CoreToolChoice::Auto,
            CoreToolChoice::Any,
            CoreToolChoice::None,
            CoreToolChoice::Tool {
                name: "get_weather".into(),
            },
            CoreToolChoice::Raw(serde_json::json!({"custom": true})),
        ];
        for variant in &variants {
            let json = serde_json::to_string(&variant).unwrap();
            let back: CoreToolChoice = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back);
        }
    }

    #[test]
    fn all_core_role_variants_round_trip() {
        let roles = vec![
            CoreRole::System,
            CoreRole::User,
            CoreRole::Assistant,
            CoreRole::Tool,
        ];
        for role in &roles {
            let msg = CoreMessage {
                role: *role,
                content: vec![CoreContent::Text {
                    text: format!("message as {role:?}"),
                    cache: None,
                }],
            };
            let json = serde_json::to_string(&msg).unwrap();
            let back: CoreMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, back);
        }
    }

    #[test]
    fn all_content_kind_variants_round_trip() {
        let kinds = vec![
            ContentKind::Text,
            ContentKind::Thinking,
            ContentKind::ToolUse,
            ContentKind::ToolResult,
            ContentKind::Image,
            ContentKind::Document,
            ContentKind::Audio,
            ContentKind::Video,
            ContentKind::Refusal,
        ];
        for kind in &kinds {
            let event = CoreEvent::ContentStart {
                index: 0,
                kind: *kind,
            };
            let json = serde_json::to_string(&event).unwrap();
            let back: CoreEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn provider_meta_with_complex_nested_json() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "deep".into(),
            serde_json::json!({
                "a": {"b": {"c": [1, 2, {"d": "value"}]}},
                "arr": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]
            }),
        );
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
            provider_meta: meta,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: CoreResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn usage_provenance_synthetic_zero_round_trips() {
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            provenance: UsageProvenance::SyntheticZero,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provenance, UsageProvenance::SyntheticZero);
        assert_eq!(usage, back);
    }

    #[test]
    fn sampling_options_stop_as_vec_string_round_trips() {
        let opts = SamplingOptions {
            stop: Some(vec!["STOP".to_owned(), "END".to_owned()]),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts).unwrap();
        let back: SamplingOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stop, Some(vec!["STOP".to_owned(), "END".to_owned()]));
    }

    #[test]
    fn thinking_delta_event_round_trips() {
        let event = CoreEvent::ThinkingDelta {
            index: 5,
            text: "I am reasoning about this...".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: CoreEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn content_start_thinking_event_round_trips() {
        let event = CoreEvent::ContentStart {
            index: 3,
            kind: ContentKind::Thinking,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: CoreEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn core_request_round_trips_with_stop_sequences() {
        let req = CoreRequest {
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions {
                stop: Some(vec!["\n".to_owned(), "END".to_owned()]),
                ..Default::default()
            },
            stream: false,
            metadata: RequestMetadata::default(),
            provider_hints: ProviderHints::default(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CoreRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
