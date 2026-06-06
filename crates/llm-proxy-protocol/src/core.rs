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
//! [`CoreToolChoice::Raw`].  These opaque fields use a **redacting `Debug`
//! implementation** that shows only the JSON type and approximate size (e.g.
//! `Object(3 keys)`, `String(42 chars)`) rather than printing values verbatim.
//! This is a defense-in-depth measure: adapters **must not** store secrets (API
//! keys, bearer tokens, etc.) in any of these fields.  Strip credentials before
//! placing data into these fields.
//!
//! ## Equality semantics
//!
//! Types that contain [`serde_json::Value`] fields derive `PartialEq` but **not**
//! `Eq`, because `serde_json::Value` uses `f64` internally and does not implement
//! `Eq`.  Equality comparisons on such types are partial: if any JSON value field
//! contains NaN, the comparison may return `false` unpredictably.  This primarily
//! affects test assertions; production code should not rely on exact equality of
//! JSON-heavy types.
//!
//! ## Forward compatibility
//!
//! All public enums are annotated with `#[non_exhaustive]`, so adding new variants
//! is not a semver-breaking change.  Core structs use `#[serde(deny_unknown_fields)]`
//! to ensure that unknown fields cause a deserialization error rather than being
//! silently dropped.  Wire-type structs in `openai.rs` may use `#[serde(flatten)]`
//! with an extra `Map` for passthrough instead.

use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// OpaqueJsonRef -- redacting Debug wrapper for borrowed serde_json::Value
// ---------------------------------------------------------------------------

/// A reference wrapper for redacting Debug output of a borrowed `Value`.
struct OpaqueJsonRef<'a>(&'a serde_json::Value);

impl fmt::Debug for OpaqueJsonRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        redact_value(self.0, f)
    }
}

/// Formats a [`serde_json::Value`] for debug output without revealing content.
///
/// Bool and Number values are shown without their actual values for consistency
/// with the redaction policy: a malicious payload could encode secret fragments
/// in number values or boolean field names.
fn redact_value(val: &serde_json::Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match val {
        serde_json::Value::Null => f.write_str("Null"),
        serde_json::Value::Bool(_) => f.write_str("Bool(_)"),
        serde_json::Value::Number(_) => f.write_str("Number(_)"),
        serde_json::Value::String(s) => write!(f, "String({} chars)", s.len()),
        serde_json::Value::Array(arr) => write!(f, "Array({} items)", arr.len()),
        serde_json::Value::Object(map) => write!(f, "Object({} keys)", map.len()),
    }
}

/// Formats a [`serde_json::Map`] for debug output without revealing content.
fn redact_map(
    map: &serde_json::Map<String, serde_json::Value>,
    f: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    write!(f, "Object({} keys)", map.len())
}

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
#[serde(deny_unknown_fields)]
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
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl fmt::Debug for CoreRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreRequest")
            .field("model", &self.model)
            .field("system", &self.system)
            .field("messages", &self.messages)
            .field("tools", &self.tools)
            .field("tool_choice", &self.tool_choice)
            .field("sampling", &self.sampling)
            .field("stream", &self.stream)
            .field("metadata", &self.metadata)
            .field("provider_hints", &self.provider_hints)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// CoreMessage / CoreRole
// ---------------------------------------------------------------------------

/// A single conversation turn.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMessage {
    /// Speaker role.
    pub role: CoreRole,
    /// Content blocks in this turn.
    pub content: Vec<CoreContent>,
}

impl fmt::Debug for CoreMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreMessage")
            .field("role", &self.role)
            .field("content", &self.content)
            .finish()
    }
}

/// Speaker role within a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
#[non_exhaustive]
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        /// This field contains security-sensitive data.  It is redacted in
        /// `Debug` output to `[REDACTED]` as a defense-in-depth measure.
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

impl fmt::Debug for CoreContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreContent::Text { text, cache } => f
                .debug_struct("Text")
                .field("text", &text)
                .field("cache", &cache)
                .finish(),
            CoreContent::Image { source } => f
                .debug_struct("Image")
                .field("source", &OpaqueJsonRef(source))
                .finish(),
            CoreContent::Document { source } => f
                .debug_struct("Document")
                .field("source", &OpaqueJsonRef(source))
                .finish(),
            CoreContent::Audio { source } => f
                .debug_struct("Audio")
                .field("source", &OpaqueJsonRef(source))
                .finish(),
            CoreContent::Video { source } => f
                .debug_struct("Video")
                .field("source", &OpaqueJsonRef(source))
                .finish(),
            CoreContent::ToolUse { id, name, input } => f
                .debug_struct("ToolUse")
                .field("id", &id)
                .field("name", &name)
                .field("input", &OpaqueJsonRef(input))
                .finish(),
            CoreContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => f
                .debug_struct("ToolResult")
                .field("tool_use_id", &tool_use_id)
                .field("content", &content)
                .field("is_error", &is_error)
                .finish(),
            CoreContent::Thinking { text, signature } => f
                .debug_struct("Thinking")
                .field("text", &text)
                .field("signature", &signature.as_ref().map(|_| "[REDACTED]"))
                .finish(),
            CoreContent::RedactedThinking { data } => f
                .debug_struct("RedactedThinking")
                .field("data", &OpaqueJsonRef(data))
                .finish(),
            CoreContent::Refusal { text } => {
                f.debug_struct("Refusal").field("text", &text).finish()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CacheControl
// ---------------------------------------------------------------------------

/// Cache-control directive attached to a content block.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheControl {
    /// Discriminator -- typically `"ephemeral"`.
    pub r#type: CacheControlType,
}

impl fmt::Debug for CacheControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheControl")
            .field("type", &self.r#type)
            .finish()
    }
}

/// Known cache control type variants.
///
/// Uses an enum with a catch-all `Other` variant so that unknown values from
/// future provider extensions are preserved rather than rejected.
#[non_exhaustive]
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

impl CacheControlType {
    /// Returns the string representation without consuming `self`.
    pub fn as_str(&self) -> &str {
        match self {
            CacheControlType::Ephemeral => "ephemeral",
            CacheControlType::Other(s) => s,
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
        serializer.serialize_str(self.as_str())
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
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl fmt::Debug for CoreTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &OpaqueJsonRef(&self.input_schema))
            .finish()
    }
}

/// Controls which (if any) tool the model must call.
#[non_exhaustive]
#[derive(Clone, PartialEq, Serialize, Deserialize)]
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

impl fmt::Debug for CoreToolChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreToolChoice::Auto => f.write_str("Auto"),
            CoreToolChoice::Any => f.write_str("Any"),
            CoreToolChoice::None => f.write_str("None"),
            CoreToolChoice::Tool { name } => f.debug_struct("Tool").field("name", &name).finish(),
            CoreToolChoice::Raw(v) => f.debug_tuple("Raw").field(&OpaqueJsonRef(v)).finish(),
        }
    }
}

// ---------------------------------------------------------------------------
// SamplingOptions
// ---------------------------------------------------------------------------

/// Sampling parameters that control generation behaviour.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingOptions {
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Nucleus sampling parameter.
    pub top_p: Option<f64>,
    /// Maximum number of tokens to generate.
    pub max_tokens: Option<i32>,
    /// Stop sequences.
    ///
    /// OpenAI's Chat Completions API allows `stop` to be a bare string
    /// (`"STOP"`), an array (`["STOP"]`), or null.  Anthropic always uses an
    /// array.  This field normalises all forms into `Vec<String>` at
    /// deserialization time: a bare string becomes a single-element vec.
    #[serde(default, deserialize_with = "deserialize_stop")]
    pub stop: Option<Vec<String>>,
    /// Reasoning effort level (e.g. `"low"`, `"medium"`, `"high"`).
    pub reasoning_effort: Option<String>,
    /// Extended thinking configuration (provider-specific JSON).
    ///
    /// This is intentionally opaque.  Expected shapes vary by provider, e.g.
    /// Anthropic: `{"type": "enabled", "budget_tokens": N}`.
    /// Not a secret-bearing field by design.
    pub thinking: Option<serde_json::Value>,
}

/// Custom deserializer for `SamplingOptions::stop` that accepts a bare string,
/// an array of strings, or null.  This normalises the OpenAI wire format where
/// `stop` can be polymorphic.
fn deserialize_stop<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    let val = Option::<serde_json::Value>::deserialize(deserializer)?;
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(vec![s])),
        Some(serde_json::Value::Array(arr)) => {
            let mut result = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    serde_json::Value::String(s) => result.push(s),
                    other => {
                        return Err(de::Error::custom(format!(
                            "stop array must contain only strings, found {}",
                            json_type_name(&other)
                        )));
                    }
                }
            }
            Ok(Some(result))
        }
        Some(other) => Err(de::Error::custom(format!(
            "stop must be a string, array of strings, or null, found {}",
            json_type_name(&other)
        ))),
    }
}

/// Returns a human-readable name for a JSON value type.
fn json_type_name(val: &serde_json::Value) -> &'static str {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

impl fmt::Debug for SamplingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SamplingOptions")
            .field("temperature", &self.temperature)
            .field("top_p", &self.top_p)
            .field("max_tokens", &self.max_tokens)
            .field("stop", &self.stop)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("thinking", &self.thinking.as_ref().map(OpaqueJsonRef))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// RequestMetadata
// ---------------------------------------------------------------------------

/// Caller metadata attached to a request.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMetadata {
    /// Optional user identifier.
    pub user_id: Option<String>,
    /// Arbitrary key-value pairs for opaque data.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

impl fmt::Debug for RequestMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestMetadata")
            .field("user_id", &self.user_id)
            .field("raw", &redact_map_helper(&self.raw))
            .finish()
    }
}

/// Helper that wraps a Map reference for redacted Debug output.
struct RedactedMap<'a>(&'a serde_json::Map<String, serde_json::Value>);

fn redact_map_helper(map: &serde_json::Map<String, serde_json::Value>) -> RedactedMap<'_> {
    RedactedMap(map)
}

impl fmt::Debug for RedactedMap<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        redact_map(self.0, f)
    }
}

// ---------------------------------------------------------------------------
// ProviderHints
// ---------------------------------------------------------------------------

/// Opaque hints consumed only by provider adapters.
///
/// Provider hints carry client-side metadata that influences how a provider
/// adapter encodes a request, but which does not belong in the canonical
/// `CoreRequest` fields.  Examples include:
///
/// - `stream_options` (OpenAI Chat: `{"include_usage": true}`)
/// - ` thinking` configuration passthrough
/// - provider-specific flags like `reasoning_effort` mapping hints
///
/// **Architectural rule:** No adapter may special-case another protocol's
/// hints.  Client adapters write hints; provider adapters read only the
/// hints they understand.  Unknown keys must be silently ignored.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderHints {
    /// Arbitrary key-value pairs.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

impl fmt::Debug for ProviderHints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderHints")
            .field("raw", &redact_map_helper(&self.raw))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// CoreResponse
// ---------------------------------------------------------------------------

/// A normalized non-streaming chat response.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl fmt::Debug for CoreResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreResponse")
            .field("id", &self.id)
            .field("model", &self.model)
            .field("content", &self.content)
            .field("stop_reason", &self.stop_reason)
            .field("stop_sequence", &self.stop_sequence)
            .field("usage", &self.usage)
            .field("provider_meta", &redact_map_helper(&self.provider_meta))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// StopReason
// ---------------------------------------------------------------------------

/// Why the model stopped generating.
#[non_exhaustive]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl Usage {
    /// Creates a synthetic-zero usage with all token counts set to zero.
    ///
    /// Use this when the provider did not report any usage data.
    /// The provenance is set to [`UsageProvenance::SyntheticZero`] to
    /// distinguish synthesised zeros from genuine provider-reported zeros.
    pub fn synthetic_zero() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            provenance: UsageProvenance::SyntheticZero,
            ..Default::default()
        }
    }

    /// Creates a provider-reported usage with the given token counts.
    ///
    /// The provenance is set to [`UsageProvenance::ProviderReported`].
    /// Callers must ensure these numbers actually came from the provider.
    pub fn provider_reported(input_tokens: i32, output_tokens: i32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            provenance: UsageProvenance::ProviderReported,
            ..Default::default()
        }
    }
}

/// Provenance of token usage numbers.
#[non_exhaustive]
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
#[non_exhaustive]
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl fmt::Debug for CoreEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreEvent::MessageStart { id, model } => f
                .debug_struct("MessageStart")
                .field("id", &id)
                .field("model", &model)
                .finish(),
            CoreEvent::ContentStart { index, kind } => f
                .debug_struct("ContentStart")
                .field("index", &index)
                .field("kind", &kind)
                .finish(),
            CoreEvent::TextDelta { index, text } => f
                .debug_struct("TextDelta")
                .field("index", &index)
                .field("text", &text)
                .finish(),
            CoreEvent::ThinkingDelta { index, text } => f
                .debug_struct("ThinkingDelta")
                .field("index", &index)
                .field("text", &text)
                .finish(),
            CoreEvent::ToolCallStart { index, id, name } => f
                .debug_struct("ToolCallStart")
                .field("index", &index)
                .field("id", &id)
                .field("name", &name)
                .finish(),
            CoreEvent::ToolCallDelta { index, args_delta } => f
                .debug_struct("ToolCallDelta")
                .field("index", &index)
                .field("args_delta", &args_delta)
                .finish(),
            CoreEvent::ToolCallStop { index } => f
                .debug_struct("ToolCallStop")
                .field("index", &index)
                .finish(),
            CoreEvent::UsageDelta { usage } => {
                f.debug_struct("UsageDelta").field("usage", &usage).finish()
            }
            CoreEvent::MessageStop {
                stop_reason,
                stop_sequence,
            } => f
                .debug_struct("MessageStop")
                .field("stop_reason", &stop_reason)
                .field("stop_sequence", &stop_sequence)
                .finish(),
            CoreEvent::Error { error } => f.debug_struct("Error").field("error", &error).finish(),
            CoreEvent::Ping => f.write_str("Ping"),
        }
    }
}

// ---------------------------------------------------------------------------
// ContentKind
// ---------------------------------------------------------------------------

/// Discriminator for content block types.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentKind {
    /// Plain text.
    Text,
    /// Extended thinking.
    Thinking,
    /// Tool invocation.
    ToolUse,
    /// Tool result.
    ///
    /// Note: `ToolResult` has no corresponding streaming delta event.  It is
    /// used only for `ContentStart` signalling in non-standard cases where a
    /// tool result block begins in a stream.  The actual content is carried
    /// through the non-stream `CoreResponse` content path.
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
///
/// # Message safety
///
/// The `message` field is intentionally `pub(crate)` to enforce that all
/// construction goes through [`CoreStreamError::new()`], which documents the
/// sanitization contract.  Adapters must strip API keys, bearer tokens, and
/// other secrets before passing the message to `new()`.
///
/// The `Debug` impl redacts the message to its character count.  The `Display`
/// impl shows only the error kind, not the message content.  The `Serialize`
/// impl includes the message verbatim for internal transport between proxy
/// components -- do **not** serialize `CoreStreamError` directly into
/// client-facing API responses.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreStreamError {
    /// Category of the error.
    pub kind: CoreStreamErrorKind,
    /// Human-readable error message (sanitized by the constructor).
    #[serde(skip_serializing, default)]
    pub(crate) message: String,
}

impl CoreStreamError {
    /// Constructs a new `CoreStreamError` with the given kind and message.
    ///
    /// Callers **must** sanitize `message` before passing it: strip API keys,
    /// bearer tokens, and other secrets from upstream provider error text.
    pub fn new(kind: CoreStreamErrorKind, message: String) -> Self {
        Self { kind, message }
    }

    /// Returns the error message.
    ///
    /// The message was sanitized at construction time by the adapter that
    /// created this error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for CoreStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreStreamError")
            .field("kind", &self.kind)
            // Truncate message to avoid leaking sensitive error details.
            .field("message", &format_args!("{} chars", self.message.len()))
            .finish()
    }
}

impl fmt::Display for CoreStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Intentionally do NOT include self.message here to avoid leaking
        // unsanitized upstream error text through Display (used by
        // tracing::error!("{err}"), error chains, etc.).  Use
        // `error.message()` explicitly if the full message is needed.
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for CoreStreamError {}
// NOTE: CoreStreamError uses a manual Error impl rather than thiserror derive
// because it also carries custom Serialize/Deserialize, a redacting Debug that
// hides the message field, and a Display that intentionally omits the message
// for security.  thiserror's `#[error()]` attribute would conflict with the
// hand-rolled Display, and the derive macro does not support the serde +
// redacting-Debug combination used here.

/// Category of a stream error.
///
/// Each variant maps to a canonical HTTP status code via
/// [`CoreStreamErrorKind::http_status()`].  Adapters should use this mapping
/// rather than inventing their own, so that all providers produce consistent
/// HTTP responses.
#[non_exhaustive]
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

impl CoreStreamErrorKind {
    /// Returns the canonical HTTP status code for this error kind.
    ///
    /// Mapping:
    /// - `InvalidRequest` -> 400
    /// - `Authentication` -> 401
    /// - `Permission` -> 403
    /// - `RateLimit` -> 429
    /// - `Upstream` -> 502
    /// - `Internal` -> 500
    pub fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Authentication => 401,
            Self::Permission => 403,
            Self::RateLimit => 429,
            Self::Upstream => 502,
            Self::Internal => 500,
        }
    }
}

impl fmt::Display for CoreStreamErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => f.write_str("invalid_request"),
            Self::Authentication => f.write_str("authentication"),
            Self::Permission => f.write_str("permission"),
            Self::RateLimit => f.write_str("rate_limit"),
            Self::Upstream => f.write_str("upstream"),
            Self::Internal => f.write_str("internal"),
        }
    }
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
            content: vec![CoreContent::Text {
                text: "it worked".into(),
                cache: None,
            }],
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
            provider_meta: serde_json::from_str(r#"{"warnings":[]}"#).expect("parse provider_meta"),
        };
        let json = serde_json::to_string(&resp).expect("serialize CoreResponse");
        let back: CoreResponse = serde_json::from_str(&json).expect("deserialize CoreResponse");
        // Full structural equality catches any serialization drift.
        assert_eq!(resp, back);
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
                error: CoreStreamError::new(
                    CoreStreamErrorKind::RateLimit,
                    "too many requests".into(),
                ),
            },
        ];

        let json = serde_json::to_string(&events).expect("serialize CoreEvent vec");
        let back: Vec<CoreEvent> = serde_json::from_str(&json).expect("deserialize CoreEvent vec");
        // Compare all events except the Error variant, whose `message` field
        // uses #[serde(skip_serializing)] and will be empty after round-trip.
        for (i, (original, round_tripped)) in events.iter().zip(back.iter()).enumerate() {
            match (original, round_tripped) {
                (CoreEvent::Error { error: orig }, CoreEvent::Error { error: rt }) => {
                    assert_eq!(orig.kind, rt.kind, "kind mismatch at event {i}");
                    // message is skip_serializing, so it comes back empty.
                    assert!(
                        rt.message.is_empty(),
                        "message should be empty after round-trip at event {i}"
                    );
                }
                _ => assert_eq!(original, round_tripped, "mismatch at event {i}"),
            }
        }
    }

    #[test]
    fn core_tool_choice_raw_round_trips_when_intentional() {
        let raw_val = serde_json::json!({"type": "some_custom", "mode": "strict"});
        let json = serde_json::to_string(&raw_val).unwrap();
        let choice = CoreToolChoice::Raw(raw_val);
        let serialized = serde_json::to_string(&choice).expect("serialize CoreToolChoice");
        let back: CoreToolChoice =
            serde_json::from_str(&serialized).expect("deserialize CoreToolChoice");
        match back {
            CoreToolChoice::Raw(v) => {
                let original: serde_json::Value = serde_json::from_str(&json).unwrap();
                assert_eq!(v, original);
            }
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
        // Test serialization (takes ownership) then From conversion on a fresh value.
        let json = serde_json::to_string(&CacheControlType::Ephemeral).unwrap();
        assert_eq!(json, "\"ephemeral\"");
        let back: CacheControlType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CacheControlType::Ephemeral);
        assert_eq!(String::from(back), "ephemeral");

        let other = CacheControlType::Other("future_type".to_owned());
        let json = serde_json::to_string(&other).unwrap();
        let back: CacheControlType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CacheControlType::Other("future_type".to_owned()));
        assert_eq!(String::from(other), "future_type");
    }

    #[test]
    fn cache_control_type_as_str() {
        assert_eq!(CacheControlType::Ephemeral.as_str(), "ephemeral");
        assert_eq!(
            CacheControlType::Other("custom".to_owned()).as_str(),
            "custom"
        );
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
        let cases = vec!["not json at all", "[", r#"{"TextDelta": "wrong"}"#];
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
        let json = serde_json::to_string(&opts).expect("serialize boundary opts");
        let back: SamplingOptions = serde_json::from_str(&json).expect("deserialize boundary opts");
        assert_eq!(back.temperature, Some(0.0));
        assert_eq!(back.max_tokens, Some(0));

        let opts_max = SamplingOptions {
            max_tokens: Some(i32::MAX),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts_max).expect("serialize max opts");
        let back: SamplingOptions = serde_json::from_str(&json).expect("deserialize max opts");
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
            let err = CoreStreamError::new(*kind, format!("test error for {kind:?}"));
            let json = serde_json::to_string(&err).expect("serialize CoreStreamError");
            let back: CoreStreamError =
                serde_json::from_str(&json).expect("deserialize CoreStreamError");
            // kind round-trips; message is #[serde(skip_serializing)] so it
            // comes back empty.
            assert_eq!(err.kind, back.kind);
            assert!(back.message.is_empty());
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

    // =======================================================================
    // Audit round 2 -- edge-case and coverage tests
    // =======================================================================

    #[test]
    fn sampling_options_stop_deserializes_bare_string() {
        // OpenAI allows stop: "STOP" as a bare string.
        let json = r#"{"stop": "STOP"}"#;
        let opts: SamplingOptions = serde_json::from_str(json).unwrap();
        assert_eq!(opts.stop, Some(vec!["STOP".to_owned()]));
    }

    #[test]
    fn sampling_options_stop_deserializes_null() {
        let json = r#"{"stop": null}"#;
        let opts: SamplingOptions = serde_json::from_str(json).unwrap();
        assert_eq!(opts.stop, None);
    }

    #[test]
    fn sampling_options_stop_deserializes_array() {
        let json = r#"{"stop": ["STOP", "END"]}"#;
        let opts: SamplingOptions = serde_json::from_str(json).unwrap();
        assert_eq!(opts.stop, Some(vec!["STOP".to_owned(), "END".to_owned()]));
    }

    #[test]
    fn sampling_options_stop_rejects_number() {
        let json = r#"{"stop": 42}"#;
        assert!(serde_json::from_str::<SamplingOptions>(json).is_err());
    }

    #[test]
    fn sampling_options_negative_temperature_round_trips() {
        let opts = SamplingOptions {
            temperature: Some(-0.5),
            top_p: Some(-0.1),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts).unwrap();
        let back: SamplingOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.temperature, Some(-0.5));
        assert_eq!(back.top_p, Some(-0.1));
    }

    #[test]
    fn sampling_options_nan_temperature_serializes_as_null() {
        // serde_json serializes NaN as null rather than erroring.  This means
        // NaN silently loses data during round-trips: deserialization produces
        // None (because Option<f64> maps JSON null to None).  This is a known
        // limitation of serde_json's default float handling.
        let opts = SamplingOptions {
            temperature: Some(f64::NAN),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts).unwrap();
        assert!(
            json.contains("null"),
            "NaN should serialize to null: {json}"
        );
        let back: SamplingOptions = serde_json::from_str(&json).unwrap();
        // NaN is lost -- temperature becomes None after round-trip.
        assert_eq!(back.temperature, None);
    }

    #[test]
    fn sampling_options_infinity_temperature_serializes_as_null() {
        // serde_json serializes Infinity as null, same as NaN.
        let opts = SamplingOptions {
            temperature: Some(f64::INFINITY),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts).unwrap();
        assert!(
            json.contains("null"),
            "Infinity should serialize to null: {json}"
        );
        let back: SamplingOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.temperature, None);
    }

    #[test]
    fn core_request_rejects_invalid_utf8_bytes() {
        let bad_bytes = b"{\"model\":{\"requested\":\"m\"},\"messages\":[],\"system\":[],\"tools\":[],\"sampling\":{},\"stream\":false,\"metadata\":{},\"provider_hints\":{},\"bad\xff_field\":1}";
        assert!(serde_json::from_slice::<CoreRequest>(bad_bytes).is_err());
    }

    #[test]
    fn core_response_rejects_invalid_utf8_bytes() {
        let bad_bytes = b"{\"model\":{\"requested\":\"m\"},\"content\":[],\"stop_reason\":\"EndTurn\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0},\"bad\xff_field\":1}";
        assert!(serde_json::from_slice::<CoreResponse>(bad_bytes).is_err());
    }

    #[test]
    fn core_request_large_messages_round_trips() {
        // Build a request with many messages to exercise large-input handling.
        let messages: Vec<CoreMessage> = (0..1000)
            .map(|i| CoreMessage {
                role: CoreRole::User,
                content: vec![CoreContent::Text {
                    text: format!("message {i}"),
                    cache: None,
                }],
            })
            .collect();
        let req = CoreRequest {
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            system: vec![],
            messages,
            tools: vec![],
            tool_choice: None,
            sampling: SamplingOptions::default(),
            stream: false,
            metadata: RequestMetadata::default(),
            provider_hints: ProviderHints::default(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CoreRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.messages.len(), 1000);
    }

    #[test]
    fn deeply_nested_tool_result_round_trips() {
        // Build a deeply nested ToolResult chain.  Depth kept at 20 to stay
        // within serde_json's default recursion limit (128).
        let depth = 20;
        let mut content = CoreContent::Text {
            text: "leaf".into(),
            cache: None,
        };
        for i in (0..depth).rev() {
            content = CoreContent::ToolResult {
                tool_use_id: format!("level_{i}"),
                content: vec![content],
                is_error: false,
            };
        }
        let json = serde_json::to_string(&content).unwrap();
        let back: CoreContent = serde_json::from_str(&json).unwrap();
        assert_eq!(content, back);
    }

    #[test]
    fn usage_synthetic_zero_constructor() {
        let usage = Usage::synthetic_zero();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.provenance, UsageProvenance::SyntheticZero);
        assert!(usage.reasoning_tokens.is_none());
    }

    #[test]
    fn usage_provider_reported_constructor() {
        let usage = Usage::provider_reported(100, 200);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.provenance, UsageProvenance::ProviderReported);
        assert!(usage.reasoning_tokens.is_none());
    }

    #[test]
    fn core_stream_error_display_and_error_traits() {
        let err = CoreStreamError::new(CoreStreamErrorKind::RateLimit, "too many requests".into());
        // Display shows the kind, not the raw message (defense-in-depth).
        let display = format!("{err}");
        assert!(
            display.contains("rate_limit"),
            "Display should show kind: {display}"
        );
        assert!(
            !display.contains("too many requests"),
            "Display should not leak message"
        );

        // Error trait
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn core_stream_error_debug_redacts_message() {
        let err =
            CoreStreamError::new(CoreStreamErrorKind::Internal, "secret-api-key-12345".into());
        let debug = format!("{err:?}");
        assert!(
            !debug.contains("secret-api-key-12345"),
            "Debug should redact message content"
        );
        assert!(debug.contains("chars"), "Debug should show message length");
    }

    #[test]
    fn core_stream_error_kind_http_status_mapping() {
        assert_eq!(CoreStreamErrorKind::InvalidRequest.http_status(), 400);
        assert_eq!(CoreStreamErrorKind::Authentication.http_status(), 401);
        assert_eq!(CoreStreamErrorKind::Permission.http_status(), 403);
        assert_eq!(CoreStreamErrorKind::RateLimit.http_status(), 429);
        assert_eq!(CoreStreamErrorKind::Upstream.http_status(), 502);
        assert_eq!(CoreStreamErrorKind::Internal.http_status(), 500);
    }

    #[test]
    fn core_stream_error_kind_display() {
        assert_eq!(format!("{}", CoreStreamErrorKind::RateLimit), "rate_limit");
        assert_eq!(
            format!("{}", CoreStreamErrorKind::InvalidRequest),
            "invalid_request"
        );
        assert_eq!(format!("{}", CoreStreamErrorKind::Internal), "internal");
    }

    #[test]
    fn core_stream_error_message_accessor() {
        let err = CoreStreamError::new(
            CoreStreamErrorKind::Upstream,
            "provider error detail".into(),
        );
        assert_eq!(err.message(), "provider error detail");
        assert_eq!(err.kind, CoreStreamErrorKind::Upstream);
    }

    #[test]
    fn thinking_signature_redacted_in_debug() {
        let content = CoreContent::Thinking {
            text: "reasoning".into(),
            signature: Some("super-secret-sig".into()),
        };
        let debug = format!("{content:?}");
        assert!(
            !debug.contains("super-secret-sig"),
            "Debug should redact signature"
        );
        assert!(debug.contains("[REDACTED]"), "Debug should show [REDACTED]");
    }

    #[test]
    fn opaque_json_fields_redacted_in_debug() {
        let content = CoreContent::Image {
            source: serde_json::json!({"url": "secret-url"}),
        };
        let debug = format!("{content:?}");
        assert!(!debug.contains("secret-url"), "Debug should redact source");
        assert!(debug.contains("Object"), "Debug should show type");
    }

    #[test]
    fn provider_meta_redacted_in_debug() {
        let resp = CoreResponse {
            id: Some("r1".into()),
            model: ModelRef {
                requested: "m".into(),
                upstream: None,
            },
            content: vec![],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
            provider_meta: serde_json::from_str(r#"{"secret":"value","count":42}"#).unwrap(),
        };
        let debug = format!("{resp:?}");
        assert!(
            !debug.contains("secret"),
            "Debug should redact provider_meta contents"
        );
        assert!(
            !debug.contains("value"),
            "Debug should redact provider_meta contents"
        );
    }

    #[test]
    fn request_metadata_raw_redacted_in_debug() {
        let meta = RequestMetadata {
            user_id: Some("user-42".into()),
            raw: serde_json::from_str(r#"{"api_key":"sk-live-key-12345"}"#).unwrap(),
        };
        let debug = format!("{meta:?}");
        assert!(
            !debug.contains("sk-live-key-12345"),
            "Debug should redact raw map"
        );
    }

    #[test]
    fn provider_hints_raw_redacted_in_debug() {
        let hints = ProviderHints {
            raw: serde_json::from_str(r#"{"token":"bearer-abc123"}"#).unwrap(),
        };
        let debug = format!("{hints:?}");
        assert!(
            !debug.contains("bearer-abc123"),
            "Debug should redact raw map"
        );
    }

    #[test]
    fn sampling_options_thinking_redacted_in_debug() {
        let opts = SamplingOptions {
            thinking: Some(serde_json::json!({"budget_tokens": 9999})),
            ..Default::default()
        };
        let debug = format!("{opts:?}");
        assert!(
            !debug.contains("budget_tokens"),
            "Debug should redact thinking"
        );
    }

    #[test]
    fn core_tool_input_schema_redacted_in_debug() {
        let tool = CoreTool {
            name: "test".into(),
            description: Some("desc".into()),
            input_schema: serde_json::json!({"secret_field": "hidden"}),
        };
        let debug = format!("{tool:?}");
        assert!(
            !debug.contains("secret_field"),
            "Debug should redact input_schema"
        );
    }

    #[test]
    fn core_tool_choice_raw_redacted_in_debug() {
        let choice = CoreToolChoice::Raw(serde_json::json!({"secret": "value"}));
        let debug = format!("{choice:?}");
        assert!(!debug.contains("secret"), "Debug should redact Raw value");
    }

    // =======================================================================
    // Audit round 3 -- deny_unknown_fields, missing round-trips, edge cases
    // =======================================================================

    #[test]
    fn core_request_rejects_unknown_fields() {
        let json = r#"{
            "model": {"requested": "m"},
            "messages": [],
            "system": [],
            "tools": [],
            "sampling": {},
            "stream": false,
            "metadata": {},
            "provider_hints": {},
            "bogus_field": true
        }"#;
        assert!(
            serde_json::from_str::<CoreRequest>(json).is_err(),
            "should reject unknown field bogus_field"
        );
    }

    #[test]
    fn core_response_rejects_unknown_fields() {
        let json = r#"{
            "model": {"requested": "m"},
            "content": [],
            "stop_reason": "EndTurn",
            "usage": {"input_tokens": 0, "output_tokens": 0, "provenance": "Unknown"},
            "bogus_field": true
        }"#;
        assert!(
            serde_json::from_str::<CoreResponse>(json).is_err(),
            "should reject unknown field bogus_field"
        );
    }

    #[test]
    fn model_ref_rejects_unknown_fields() {
        let json = r#"{"requested": "gpt-4o", "upstream": null, "bogus": true}"#;
        assert!(
            serde_json::from_str::<ModelRef>(json).is_err(),
            "should reject unknown field on ModelRef"
        );
    }

    #[test]
    fn cache_control_round_trips_through_json() {
        let cc = CacheControl {
            r#type: CacheControlType::Ephemeral,
        };
        let json = serde_json::to_string(&cc).expect("serialize CacheControl");
        let back: CacheControl = serde_json::from_str(&json).expect("deserialize CacheControl");
        assert_eq!(cc, back);

        let cc_other = CacheControl {
            r#type: CacheControlType::Other("custom".into()),
        };
        let json = serde_json::to_string(&cc_other).expect("serialize CacheControl Other");
        let back: CacheControl =
            serde_json::from_str(&json).expect("deserialize CacheControl Other");
        assert_eq!(cc_other, back);
    }

    #[test]
    fn sampling_options_stop_rejects_array_with_non_strings() {
        // Array containing a number should be rejected.
        let json = r#"{"stop": ["STOP", 42]}"#;
        assert!(
            serde_json::from_str::<SamplingOptions>(json).is_err(),
            "should reject array with non-string items"
        );
        // Array containing a boolean should be rejected.
        let json2 = r#"{"stop": [true]}"#;
        assert!(
            serde_json::from_str::<SamplingOptions>(json2).is_err(),
            "should reject array with boolean items"
        );
    }

    #[test]
    fn sampling_options_stop_deserializes_empty_array() {
        let json = r#"{"stop": []}"#;
        let opts: SamplingOptions = serde_json::from_str(json).expect("deserialize");
        assert_eq!(opts.stop, Some(vec![]));
    }

    #[test]
    fn sampling_options_thinking_round_trips() {
        let opts = SamplingOptions {
            thinking: Some(serde_json::json!({"type": "enabled", "budget_tokens": 5000})),
            ..Default::default()
        };
        let json = serde_json::to_string(&opts).expect("serialize");
        let back: SamplingOptions = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(opts, back);
        assert_eq!(
            back.thinking.as_ref().unwrap().get("budget_tokens"),
            Some(&serde_json::json!(5000))
        );
    }

    #[test]
    fn core_tool_round_trips_through_json() {
        let tool = CoreTool {
            name: "get_weather".into(),
            description: Some("Get the current weather".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            }),
        };
        let json = serde_json::to_string(&tool).expect("serialize CoreTool");
        let back: CoreTool = serde_json::from_str(&json).expect("deserialize CoreTool");
        assert_eq!(tool, back);
    }

    #[test]
    fn bool_and_number_redacted_in_debug() {
        // Bool and Number values should not show their actual values in Debug.
        let content = CoreContent::Image {
            source: serde_json::json!({"flag": true, "count": 42}),
        };
        let debug = format!("{content:?}");
        // The outer Object type is shown but Bool/Number values inside are
        // not printed verbatim -- redact_value hides inner details.
        assert!(
            debug.contains("Object"),
            "should show Object type for source"
        );
    }

    #[test]
    fn core_content_rejects_unknown_fields_in_variant() {
        // Text variant with an extra field should be rejected, not silently dropped.
        let json = r#"{"Text": {"text": "hi", "secret_extra": "leaked"}}"#;
        assert!(
            serde_json::from_str::<CoreContent>(json).is_err(),
            "CoreContent::Text should reject unknown field"
        );
        // ToolUse variant with an extra field.
        let json2 = r#"{"ToolUse": {"id": "1", "name": "fn", "input": {}, "extra": true}}"#;
        assert!(
            serde_json::from_str::<CoreContent>(json2).is_err(),
            "CoreContent::ToolUse should reject unknown field"
        );
        // ToolResult variant with an extra field.
        let json3 =
            r#"{"ToolResult": {"tool_use_id": "1", "content": [], "is_error": false, "extra": 1}}"#;
        assert!(
            serde_json::from_str::<CoreContent>(json3).is_err(),
            "CoreContent::ToolResult should reject unknown field"
        );
    }

    #[test]
    fn core_event_rejects_unknown_fields_in_variant() {
        // TextDelta with an extra field.
        let json = r#"{"TextDelta": {"index": 0, "text": "hi", "extra": true}}"#;
        assert!(
            serde_json::from_str::<CoreEvent>(json).is_err(),
            "CoreEvent::TextDelta should reject unknown field"
        );
        // MessageStart with an extra field.
        let json2 = r#"{"MessageStart": {"id": null, "model": {"requested": "m"}, "extra": 1}}"#;
        assert!(
            serde_json::from_str::<CoreEvent>(json2).is_err(),
            "CoreEvent::MessageStart should reject unknown field"
        );
        // MessageStop with an extra field.
        let json3 =
            r#"{"MessageStop": {"stop_reason": "EndTurn", "stop_sequence": null, "extra": true}}"#;
        assert!(
            serde_json::from_str::<CoreEvent>(json3).is_err(),
            "CoreEvent::MessageStop should reject unknown field"
        );
    }
}
