//! Provider protocol adapters: [`CoreRequest`] -> provider wire format and back.
//!
//! Each adapter handles exactly one provider protocol:
//!
//! - [`OpenAiChatAdapter`] -- OpenAI Chat Completions API
//! - [`AnthropicAdapter`]  -- Anthropic Messages API
//! - [`ResponsesAdapter`]  -- OpenAI Responses API
//! - [`GeminiAdapter`]     -- Google Gemini GenerateContent API
//!
//! Adapters translate between the normalized core types ([`CoreRequest`],
//! [`CoreResponse`], [`CoreEvent`]) and the provider-specific wire types. They
//! never import client adapters, route handlers, or server state.

pub mod anthropic;
pub mod gemini;
pub mod openai_chat;
pub mod responses;

pub use anthropic::AnthropicAdapter;
pub use gemini::GeminiAdapter;
pub use openai_chat::OpenAiChatAdapter;
pub use responses::ResponsesAdapter;

use std::collections::HashMap;
use std::fmt;

use llm_proxy_core::AuthStyle;
use llm_proxy_protocol::core::{
    CoreEvent, CoreRequest, CoreResponse, ModelRef, StopReason, Usage, UsageProvenance,
};

use crate::error::ProviderError;
use crate::sse::SseFrame;
use crate::transport::{AuthHeaders, ProxyRequest};

/// Anthropic API version header value used when the caller does not supply one.
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ---------------------------------------------------------------------------
// ProviderProtocol
// ---------------------------------------------------------------------------

/// Identifies a provider protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderProtocol {
    /// OpenAI Chat Completions API (`/v1/chat/completions`).
    OpenAiChatCompletions,
    /// Anthropic Messages API (`/v1/messages`).
    AnthropicMessages,
    /// OpenAI Responses API (`/v1/responses`).
    OpenAiResponses,
    /// Google Gemini GenerateContent API (`/v1beta/models/{model}:generateContent`).
    GeminiGenerateContent,
}

impl ProviderProtocol {
    /// Returns the canonical string name for this protocol.
    ///
    /// Names use snake_case to match the provider TOML convention
    /// (e.g. `openai_chat_completions`, `anthropic_messages`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
            Self::OpenAiResponses => "openai_responses",
            Self::GeminiGenerateContent => "gemini_generate_content",
        }
    }

    /// Parse a protocol name string (case-insensitive).
    ///
    /// Accepts canonical snake_case protocol names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "openai_chat_completions" => Some(Self::OpenAiChatCompletions),
            "anthropic_messages" => Some(Self::AnthropicMessages),
            "openai_responses" => Some(Self::OpenAiResponses),
            "gemini_generate_content" => Some(Self::GeminiGenerateContent),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderAdapterTarget
// ---------------------------------------------------------------------------

/// Everything an adapter needs to encode a request for a specific provider.
///
/// The `api_key` field is redacted in [`fmt::Debug`] output so that
/// `tracing::debug!(?target)` or snapshot output never leaks the secret.
#[derive(Clone)]
pub struct ProviderAdapterTarget {
    /// Logical provider name (for logging/metrics).
    pub provider_name: String,
    /// Adapter name from config.
    pub adapter_name: String,
    /// Which protocol this adapter speaks.
    pub protocol: ProviderProtocol,
    /// Full upstream endpoint URL (possibly with `{model}` placeholder).
    pub endpoint: String,
    /// How to authenticate with the upstream.
    pub auth_style: AuthStyle,
    /// API key for the upstream. Redacted in Debug output.
    pub api_key: String,
    /// The model the client asked for.
    pub requested_model: String,
    /// The model to send upstream (may differ due to aliasing).
    pub upstream_model: String,
    /// Optional static headers from the adapter config (e.g. `anthropic-version`).
    pub headers: std::collections::HashMap<String, String>,
}

impl fmt::Debug for ProviderAdapterTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderAdapterTarget")
            .field("provider_name", &self.provider_name)
            .field("adapter_name", &self.adapter_name)
            .field("protocol", &self.protocol)
            .field("endpoint", &endpoint_without_query(&self.endpoint))
            .field("auth_style", &self.auth_style)
            .field("api_key", &"[REDACTED]")
            .field("requested_model", &self.requested_model)
            .field("upstream_model", &self.upstream_model)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

pub(crate) use crate::transport::endpoint_without_query;

// ---------------------------------------------------------------------------
// ProviderAdapter enum
// ---------------------------------------------------------------------------

/// Enum-dispatched provider adapter.
///
/// Each variant wraps a concrete adapter that knows how to translate between
/// core types and its provider-specific wire format.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ProviderAdapter {
    /// OpenAI Chat Completions adapter.
    OpenAiChat(openai_chat::OpenAiChatAdapter),
    /// Anthropic Messages adapter.
    Anthropic(anthropic::AnthropicAdapter),
    /// OpenAI Responses API adapter.
    Responses(responses::ResponsesAdapter),
    /// Google Gemini GenerateContent adapter.
    Gemini(gemini::GeminiAdapter),
}

impl ProviderAdapter {
    /// Returns the protocol this adapter handles.
    #[must_use]
    pub fn protocol(&self) -> ProviderProtocol {
        match self {
            Self::OpenAiChat(_) => ProviderProtocol::OpenAiChatCompletions,
            Self::Anthropic(_) => ProviderProtocol::AnthropicMessages,
            Self::Responses(_) => ProviderProtocol::OpenAiResponses,
            Self::Gemini(_) => ProviderProtocol::GeminiGenerateContent,
        }
    }

    /// Encode a `CoreRequest` into a transport-ready `ProxyRequest`.
    pub fn encode_request(
        &self,
        core: &CoreRequest,
        target: &ProviderAdapterTarget,
    ) -> Result<ProxyRequest, ProviderError> {
        // Log non-empty provider_hints so they are not silently ignored.
        // Per the plan's lossy translation rules, hints with no provider mapping
        // should be warned at warn level.  No adapter currently forwards any
        // hint keys, so all non-empty hints are unmapped.
        if !core.provider_hints.raw.is_empty() {
            tracing::warn!(
                protocol = %target.protocol.name(),
                "provider_hints present but no adapter currently forwards them"
            );
        }
        match self {
            Self::OpenAiChat(a) => a.encode_request(core, target),
            Self::Anthropic(a) => a.encode_request(core, target),
            Self::Responses(a) => a.encode_request(core, target),
            Self::Gemini(a) => a.encode_request(core, target),
        }
    }

    /// Decode a non-streaming provider response body into a `CoreResponse`.
    pub fn decode_response(
        &self,
        bytes: &[u8],
        target: &ProviderAdapterTarget,
    ) -> Result<CoreResponse, ProviderError> {
        match self {
            Self::OpenAiChat(a) => a.decode_response(bytes, target),
            Self::Anthropic(a) => a.decode_response(bytes, target),
            Self::Responses(a) => a.decode_response(bytes, target),
            Self::Gemini(a) => a.decode_response(bytes, target),
        }
    }

    /// Create a new stream decoder for this adapter.
    pub fn new_stream_decoder(
        &self,
        target: &ProviderAdapterTarget,
    ) -> Box<dyn ProviderStreamDecoder + Send> {
        match self {
            Self::OpenAiChat(a) => a.new_stream_decoder(target),
            Self::Anthropic(a) => a.new_stream_decoder(target),
            Self::Responses(a) => a.new_stream_decoder(target),
            Self::Gemini(a) => a.new_stream_decoder(target),
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderStreamDecoder trait
// ---------------------------------------------------------------------------

/// Decodes already-framed SSE events from an upstream provider into core events.
///
/// The decoder receives parsed [`SseFrame`] objects from [`SseFramer`](crate::sse::SseFramer).
/// It must not parse raw network chunks.
pub trait ProviderStreamDecoder: fmt::Debug {
    /// Decode a single SSE frame into zero or more core events.
    fn decode_frame(&mut self, frame: &SseFrame) -> Result<Vec<CoreEvent>, ProviderError>;

    /// Flush any remaining buffered state at stream end.
    fn finish(&mut self) -> Result<Vec<CoreEvent>, ProviderError>;
}

// ---------------------------------------------------------------------------
// ProviderAdapterRegistry
// ---------------------------------------------------------------------------

/// Registry of available provider adapters, keyed by protocol.
#[derive(Debug, Clone)]
pub struct ProviderAdapterRegistry {
    adapters: HashMap<ProviderProtocol, ProviderAdapter>,
}

impl ProviderAdapterRegistry {
    /// Create a registry with all built-in adapters.
    pub fn builtin() -> Self {
        let mut adapters = HashMap::new();
        adapters.insert(
            ProviderProtocol::OpenAiChatCompletions,
            ProviderAdapter::OpenAiChat(openai_chat::OpenAiChatAdapter),
        );
        adapters.insert(
            ProviderProtocol::AnthropicMessages,
            ProviderAdapter::Anthropic(anthropic::AnthropicAdapter::new()),
        );
        adapters.insert(
            ProviderProtocol::OpenAiResponses,
            ProviderAdapter::Responses(responses::ResponsesAdapter),
        );
        adapters.insert(
            ProviderProtocol::GeminiGenerateContent,
            ProviderAdapter::Gemini(gemini::GeminiAdapter),
        );
        Self { adapters }
    }

    /// Returns the canonical protocol names.
    #[must_use]
    pub fn protocol_names(&self) -> Vec<&'static str> {
        self.adapters.keys().map(|p| p.name()).collect()
    }

    /// Returns `true` if the given protocol name is registered.
    pub fn has_protocol_name(&self, protocol: &str) -> bool {
        ProviderProtocol::parse(protocol)
            .map(|p| self.adapters.contains_key(&p))
            .unwrap_or(false)
    }

    /// Look up an adapter by protocol.
    pub fn get(&self, protocol: ProviderProtocol) -> Option<&ProviderAdapter> {
        self.adapters.get(&protocol)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers used by multiple adapters
// ---------------------------------------------------------------------------

/// Expand URL template placeholders.
///
/// Currently supports `{model}` -> `upstream_model`.
///
/// Validates that the model name contains only safe characters. Gemini resource
/// names may contain one leading `models/` segment; all other slashes are
/// rejected.
///
/// # Path canonicalization
///
/// The expanded URL is **not** canonicalized (no `..` resolution, no double-slash
/// collapse). This is acceptable because the model name validation above rejects
/// `..` and `/`, so the only way `..` can appear in the URL is via the template
/// itself, which is a static config value controlled by the operator. If user-
/// controlled input is ever interpolated into URLs beyond the model name, path
/// canonicalization must be added here.
pub(crate) fn expand_url_template(
    template: &str,
    target: &ProviderAdapterTarget,
) -> Result<String, ProviderError> {
    if !template.contains("{model}") {
        return Ok(template.to_owned());
    }
    let configured_model = &target.upstream_model;
    let model = configured_model
        .strip_prefix("models/")
        .filter(|_| template.contains("models/{model}"))
        .unwrap_or(configured_model);
    // Reject model names containing path traversal or other unsafe characters.
    // Additionally reject `.` and `..` exactly (path traversal patterns).
    if model == ".." || model == "." {
        return Err(ProviderError::InvalidConfig(format!(
            "upstream_model {:?} is a path traversal pattern; refusing to interpolate into URL",
            model
        )));
    }
    let valid_resource_name = model
        .strip_prefix("models/")
        .is_some_and(is_safe_model_component);
    if !is_safe_model_component(model) && !valid_resource_name {
        return Err(ProviderError::InvalidConfig(format!(
            "upstream_model {:?} contains unsafe characters; refusing to interpolate into URL",
            model
        )));
    }
    Ok(template.replace("{model}", model))
}

fn is_safe_model_component(model: &str) -> bool {
    !model.is_empty()
        && model != "."
        && model != ".."
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Map a finish reason string from OpenAI-compatible providers to a core StopReason.
pub(crate) fn map_openai_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "tool_use" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

/// Map a Gemini finish reason string to a core StopReason.
///
/// SAFETY and RECITATION indicate content was filtered by safety systems.
/// These map to `StopReason::Refusal` because the model refused to complete
/// the response due to content policy, which is semantically closer to a
/// refusal than a normal end-of-turn.
pub(crate) fn map_gemini_finish_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

/// Build usage from OpenAI-style usage info, accounting for cache tokens.
pub(crate) fn build_usage_from_openai(
    prompt_tokens: i32,
    completion_tokens: i32,
    cache_hit: Option<i32>,
    cache_miss: Option<i32>,
) -> Usage {
    let cache_hit_i64 = cache_hit.unwrap_or(0) as i64;
    let cache_miss_i64 = cache_miss.unwrap_or(0) as i64;
    let input = (prompt_tokens as i64) - cache_hit_i64 - cache_miss_i64;
    let input_tokens = i32::try_from(input.max(0)).unwrap_or(i32::MAX);

    Usage {
        input_tokens,
        output_tokens: completion_tokens,
        reasoning_tokens: None,
        cache_creation_input_tokens: cache_miss,
        cache_read_input_tokens: cache_hit,
        provenance: UsageProvenance::ProviderReported,
    }
}

/// Build a `ProxyRequest` from the adapter's encoded JSON body and target info.
pub(crate) fn build_proxy_request(
    body: Vec<u8>,
    target: &ProviderAdapterTarget,
    stream: bool,
    url: String,
) -> ProxyRequest {
    let mut extra_headers = target.headers.clone();
    if target.protocol == ProviderProtocol::AnthropicMessages
        && !extra_headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("anthropic-version"))
    {
        extra_headers.insert("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned());
    }

    ProxyRequest {
        url,
        auth: AuthHeaders {
            style: target.auth_style,
            api_key: target.api_key.clone(),
        },
        body,
        stream,
        extra_headers,
    }
}

/// Build the response ModelRef preserving the client-facing requested model.
pub(crate) fn response_model_ref(target: &ProviderAdapterTarget) -> ModelRef {
    let upstream = if target.upstream_model != target.requested_model {
        Some(target.upstream_model.clone())
    } else {
        None
    };
    ModelRef {
        requested: target.requested_model.clone(),
        upstream,
    }
}

/// Truncate a string to `max_len` bytes, respecting UTF-8 char boundaries.
///
/// If the string is longer than `max_len`, finds the nearest char boundary at
/// or before `max_len`. This prevents panics when slicing strings containing
/// multi-byte UTF-8 characters (e.g. CJK text, emoji).
pub(crate) fn truncate_str_safe(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- ProviderProtocol name/parse -----------------------------------------

    #[test]
    fn protocol_name_roundtrip() {
        let protocols = [
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::AnthropicMessages,
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::GeminiGenerateContent,
        ];
        for p in &protocols {
            assert_eq!(ProviderProtocol::parse(p.name()), Some(*p));
        }
    }

    #[test]
    fn protocol_parse_case_insensitive() {
        assert_eq!(
            ProviderProtocol::parse("OpenAI_Chat_Completions"),
            Some(ProviderProtocol::OpenAiChatCompletions)
        );
        assert_eq!(
            ProviderProtocol::parse("ANTHROPIC_MESSAGES"),
            Some(ProviderProtocol::AnthropicMessages)
        );
        assert_eq!(
            ProviderProtocol::parse("Gemini_Generate_Content"),
            Some(ProviderProtocol::GeminiGenerateContent)
        );
    }

    #[test]
    fn protocol_parse_unknown_returns_none() {
        assert_eq!(ProviderProtocol::parse("unknown"), None);
        assert_eq!(ProviderProtocol::parse(""), None);
    }

    // -- Registry -----------------------------------------------------------

    #[test]
    fn registry_builtin_has_all_protocols() {
        let reg = ProviderAdapterRegistry::builtin();
        assert_eq!(reg.adapters.len(), 4);
        assert!(reg.get(ProviderProtocol::OpenAiChatCompletions).is_some());
        assert!(reg.get(ProviderProtocol::AnthropicMessages).is_some());
        assert!(reg.get(ProviderProtocol::OpenAiResponses).is_some());
        assert!(reg.get(ProviderProtocol::GeminiGenerateContent).is_some());
    }

    #[test]
    fn registry_protocol_names() {
        let reg = ProviderAdapterRegistry::builtin();
        let names = reg.protocol_names();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"openai_chat_completions"));
        assert!(names.contains(&"anthropic_messages"));
        assert!(names.contains(&"openai_responses"));
        assert!(names.contains(&"gemini_generate_content"));
    }

    #[test]
    fn registry_has_protocol_name() {
        let reg = ProviderAdapterRegistry::builtin();
        assert!(reg.has_protocol_name("openai_chat_completions"));
        assert!(reg.has_protocol_name("anthropic_messages"));
        assert!(reg.has_protocol_name("openai_responses"));
        assert!(reg.has_protocol_name("gemini_generate_content"));
        assert!(!reg.has_protocol_name("unknown"));
        assert!(!reg.has_protocol_name("openai-chat"));
        assert!(!reg.has_protocol_name("anthropic"));
    }

    // -- Adapter protocol method ---------------------------------------------

    #[test]
    fn adapter_protocol_dispatch() {
        let reg = ProviderAdapterRegistry::builtin();
        assert_eq!(
            reg.get(ProviderProtocol::OpenAiChatCompletions)
                .unwrap()
                .protocol(),
            ProviderProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            reg.get(ProviderProtocol::AnthropicMessages)
                .unwrap()
                .protocol(),
            ProviderProtocol::AnthropicMessages
        );
        assert_eq!(
            reg.get(ProviderProtocol::OpenAiResponses)
                .unwrap()
                .protocol(),
            ProviderProtocol::OpenAiResponses
        );
        assert_eq!(
            reg.get(ProviderProtocol::GeminiGenerateContent)
                .unwrap()
                .protocol(),
            ProviderProtocol::GeminiGenerateContent
        );
    }

    // -- URL template expansion ----------------------------------------------

    #[test]
    fn url_template_expands_model() {
        let target = make_target(ProviderProtocol::GeminiGenerateContent);
        let url = expand_url_template(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent",
            &target,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent"
        );
    }

    #[test]
    fn url_template_no_placeholder_unchanged() {
        let target = make_target(ProviderProtocol::OpenAiChatCompletions);
        let url =
            expand_url_template("https://api.openai.com/v1/chat/completions", &target).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn url_without_placeholder_accepts_body_only_resource_model() {
        let mut target = make_target(ProviderProtocol::OpenAiChatCompletions);
        target.upstream_model = "accounts/example/models/deepseek-v3".into();
        let url = expand_url_template(
            "https://api.fireworks.ai/inference/v1/chat/completions",
            &target,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://api.fireworks.ai/inference/v1/chat/completions"
        );
    }

    #[test]
    fn url_template_rejects_unsafe_model_name() {
        let mut target = make_target(ProviderProtocol::OpenAiChatCompletions);
        target.upstream_model = "../../etc/passwd".into();
        let result = expand_url_template("https://api.openai.com/v1/{model}", &target);
        assert!(
            result.is_err(),
            "should reject model name with path traversal"
        );
    }

    #[test]
    fn url_template_accepts_discovered_gemini_resource_name() {
        let mut target = make_target(ProviderProtocol::GeminiGenerateContent);
        target.upstream_model = "models/gemini-2.5-pro".into();
        let url = expand_url_template(
            "https://generativelanguage.googleapis.com/v1beta/{model}:generateContent",
            &target,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn url_template_does_not_duplicate_gemini_models_segment() {
        let mut target = make_target(ProviderProtocol::GeminiGenerateContent);
        target.upstream_model = "models/gemini-2.5-pro".into();
        let url = expand_url_template(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent",
            &target,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn url_template_rejects_nested_model_resource_name() {
        let mut target = make_target(ProviderProtocol::GeminiGenerateContent);
        target.upstream_model = "models/group/gemini-2.5-pro".into();
        assert!(
            expand_url_template("https://example.com/{model}", &target).is_err(),
            "only the provider-defined models/ prefix may contain a slash"
        );
    }

    #[test]
    fn provider_adapter_target_debug_redacts_api_key() {
        let target = ProviderAdapterTarget {
            provider_name: "test".into(),
            adapter_name: "openai-chat".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            endpoint: "https://api.openai.com/v1/chat/completions?key=query-secret".into(),
            auth_style: AuthStyle::Bearer,
            api_key: "sk-test-super-secret-key-1234567890".into(),
            requested_model: "gpt-4o".into(),
            upstream_model: "gpt-4o".into(),
            headers: std::collections::HashMap::new(),
        };
        let debug = format!("{:?}", target);
        assert!(
            !debug.contains("sk-test-super-secret-key-1234567890"),
            "Debug output must not contain the actual API key"
        );
        assert!(
            debug.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for api_key"
        );
        assert!(!debug.contains("query-secret"));
    }

    // -- Finish reason mapping -----------------------------------------------

    #[test]
    fn openai_finish_reason_mappings() {
        assert_eq!(map_openai_finish_reason("stop"), StopReason::EndTurn);
        assert_eq!(map_openai_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_openai_finish_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_openai_finish_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(
            map_openai_finish_reason("content_filter"),
            StopReason::Refusal
        );
        assert_eq!(map_openai_finish_reason("unknown"), StopReason::Unknown);
    }

    #[test]
    fn gemini_finish_reason_mappings() {
        assert_eq!(map_gemini_finish_reason("STOP"), StopReason::EndTurn);
        assert_eq!(
            map_gemini_finish_reason("MAX_TOKENS"),
            StopReason::MaxTokens
        );
        assert_eq!(map_gemini_finish_reason("SAFETY"), StopReason::Refusal);
        assert_eq!(map_gemini_finish_reason("RECITATION"), StopReason::Refusal);
        assert_eq!(map_gemini_finish_reason("other"), StopReason::Unknown);
    }

    // -- Usage building ------------------------------------------------------

    #[test]
    fn usage_from_openai_basic() {
        let usage = build_usage_from_openai(100, 50, Some(10), Some(20));
        assert_eq!(usage.input_tokens, 70); // 100 - 10 - 20
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, Some(10));
        assert_eq!(usage.cache_creation_input_tokens, Some(20));
        assert_eq!(usage.provenance, UsageProvenance::ProviderReported);
    }

    #[test]
    fn usage_from_openai_no_cache() {
        let usage = build_usage_from_openai(100, 50, None, None);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn usage_from_openai_clamps_to_zero() {
        let usage = build_usage_from_openai(5, 10, Some(80), Some(20));
        assert_eq!(usage.input_tokens, 0);
    }

    // -- Response model ref --------------------------------------------------

    #[test]
    fn response_model_ref_no_alias() {
        let target = ProviderAdapterTarget {
            provider_name: "test".into(),
            adapter_name: "openai-chat".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            endpoint: "https://api.openai.com/v1/chat/completions".into(),
            auth_style: AuthStyle::Bearer,
            api_key: "key".into(),
            requested_model: "gpt-4o".into(),
            upstream_model: "gpt-4o".into(),
            headers: std::collections::HashMap::new(),
        };
        let mr = response_model_ref(&target);
        assert_eq!(mr.requested, "gpt-4o");
        assert!(mr.upstream.is_none());
    }

    #[test]
    fn response_model_ref_with_alias() {
        let target = ProviderAdapterTarget {
            provider_name: "test".into(),
            adapter_name: "openai-chat".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            endpoint: "https://api.openai.com/v1/chat/completions".into(),
            auth_style: AuthStyle::Bearer,
            api_key: "key".into(),
            requested_model: "my-alias".into(),
            upstream_model: "gpt-4o-2024-08-06".into(),
            headers: std::collections::HashMap::new(),
        };
        let mr = response_model_ref(&target);
        assert_eq!(mr.requested, "my-alias");
        assert_eq!(mr.upstream.as_deref(), Some("gpt-4o-2024-08-06"));
    }

    // -- Source guard ---------------------------------------------------------

    #[test]
    fn adapter_source_no_forbidden_imports() {
        let source = include_str!("mod.rs");
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

    // -- truncate_str_safe ---------------------------------------------------

    #[test]
    fn truncate_str_safe_ascii_within_limit() {
        assert_eq!(truncate_str_safe("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_safe_ascii_at_boundary() {
        assert_eq!(truncate_str_safe("hello world", 5), "hello");
    }

    #[test]
    fn truncate_str_safe_multibyte_no_panic() {
        // Japanese 'あ' is 3 bytes.  If max_len lands mid-character, we must
        // back up to the previous char boundary.
        let s = "あいうえお"; // 15 bytes total
        // max_len=4 lands inside 'い' (bytes 3-5), so we back up to byte 3.
        let truncated = truncate_str_safe(s, 4);
        assert_eq!(truncated, "あ"); // Only first char (bytes 0-2) fits
        assert!(truncated.len() <= 4);
    }

    #[test]
    fn truncate_str_safe_empty_string() {
        assert_eq!(truncate_str_safe("", 200), "");
    }

    #[test]
    fn truncate_str_safe_zero_max_len() {
        assert_eq!(truncate_str_safe("hello", 0), "");
    }

    #[test]
    fn anthropic_requests_default_version_header() {
        let target = make_target(ProviderProtocol::AnthropicMessages);
        let request = build_proxy_request(Vec::new(), &target, false, target.endpoint.clone());

        assert_eq!(
            request.extra_headers.get("anthropic-version"),
            Some(&ANTHROPIC_VERSION.to_owned())
        );
    }

    #[test]
    fn anthropic_requests_preserve_configured_version_header() {
        let mut target = make_target(ProviderProtocol::AnthropicMessages);
        target
            .headers
            .insert("Anthropic-Version".to_owned(), "2024-01-01".to_owned());
        let request = build_proxy_request(Vec::new(), &target, false, target.endpoint.clone());

        assert_eq!(
            request.extra_headers.get("Anthropic-Version"),
            Some(&"2024-01-01".to_owned())
        );
        assert!(!request.extra_headers.contains_key("anthropic-version"));
    }

    // -- Helpers --------------------------------------------------------------

    fn make_target(protocol: ProviderProtocol) -> ProviderAdapterTarget {
        let (endpoint, auth_style) = match protocol {
            ProviderProtocol::OpenAiChatCompletions => (
                "https://api.openai.com/v1/chat/completions".into(),
                AuthStyle::Bearer,
            ),
            ProviderProtocol::AnthropicMessages => (
                "https://api.anthropic.com/v1/messages".into(),
                AuthStyle::XApiKey,
            ),
            ProviderProtocol::OpenAiResponses => (
                "https://api.openai.com/v1/responses".into(),
                AuthStyle::Bearer,
            ),
            ProviderProtocol::GeminiGenerateContent => (
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
                    .into(),
                AuthStyle::Bearer,
            ),
        };
        ProviderAdapterTarget {
            provider_name: "test".into(),
            adapter_name: protocol.name().into(),
            protocol,
            endpoint,
            auth_style,
            api_key: "test-key".into(),
            requested_model: "gpt-4o".into(),
            upstream_model: "gemini-2.5-pro".into(),
            headers: std::collections::HashMap::new(),
        }
    }
}
