//! Provider protocol adapters: CoreRequest -> provider wire format and back.
//!
//! Each adapter handles exactly one provider protocol:
//!
//! - `OpenAiChatAdapter` -- OpenAI Chat Completions API
//! - `AnthropicAdapter`  -- Anthropic Messages API
//! - `ResponsesAdapter`  -- OpenAI Responses API
//! - `GeminiAdapter`     -- Google Gemini GenerateContent API
//!
//! Adapters translate between the normalized core types (`CoreRequest`,
//! `CoreResponse`, `CoreEvent`) and the provider-specific wire types. They
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

// ---------------------------------------------------------------------------
// ProviderProtocol
// ---------------------------------------------------------------------------

/// Identifies a provider protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai-chat",
            Self::AnthropicMessages => "anthropic",
            Self::OpenAiResponses => "openai-responses",
            Self::GeminiGenerateContent => "gemini",
        }
    }

    /// Parse a protocol name string (case-insensitive).
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "openai-chat" => Some(Self::OpenAiChatCompletions),
            "anthropic" => Some(Self::AnthropicMessages),
            "openai-responses" => Some(Self::OpenAiResponses),
            "gemini" => Some(Self::GeminiGenerateContent),
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
}

impl fmt::Debug for ProviderAdapterTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderAdapterTarget")
            .field("provider_name", &self.provider_name)
            .field("adapter_name", &self.adapter_name)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field("auth_style", &self.auth_style)
            .field("api_key", &"***")
            .field("requested_model", &self.requested_model)
            .field("upstream_model", &self.upstream_model)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ProviderAdapter enum
// ---------------------------------------------------------------------------

/// Enum-dispatched provider adapter.
///
/// Each variant wraps a concrete adapter that knows how to translate between
/// core types and its provider-specific wire format.
#[derive(Debug, Clone)]
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
/// Validates that the model name contains only safe characters (alphanumeric,
/// dots, hyphens, underscores) to prevent path traversal injection.  Returns
/// an error if the model name contains characters that could enable SSRF or
/// path-traversal attacks (e.g. `/`, `..`, control characters).
pub(crate) fn expand_url_template(
    template: &str,
    target: &ProviderAdapterTarget,
) -> Result<String, ProviderError> {
    let model = &target.upstream_model;
    // Reject model names containing path traversal or other unsafe characters.
    if !model.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_') {
        return Err(ProviderError::SseFraming(format!(
            "upstream_model {:?} contains unsafe characters; refusing to interpolate into URL",
            model
        )));
    }
    Ok(template.replace("{model}", model))
}

/// Map a finish reason string from OpenAI-compatible providers to a core StopReason.
pub(crate) fn map_openai_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "tool_use" => StopReason::ToolUse,
        "content_filter" => StopReason::EndTurn,
        _ => StopReason::Unknown,
    }
}

/// Map a Gemini finish reason string to a core StopReason.
pub(crate) fn map_gemini_finish_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" => StopReason::EndTurn,
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
    ProxyRequest {
        url,
        auth: AuthHeaders {
            style: target.auth_style.clone(),
            api_key: target.api_key.clone(),
        },
        body,
        stream,
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
            ProviderProtocol::parse("OpenAI-Chat"),
            Some(ProviderProtocol::OpenAiChatCompletions)
        );
        assert_eq!(
            ProviderProtocol::parse("ANTHROPIC"),
            Some(ProviderProtocol::AnthropicMessages)
        );
        assert_eq!(
            ProviderProtocol::parse("Gemini"),
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
        assert!(names.contains(&"openai-chat"));
        assert!(names.contains(&"anthropic"));
        assert!(names.contains(&"openai-responses"));
        assert!(names.contains(&"gemini"));
    }

    #[test]
    fn registry_has_protocol_name() {
        let reg = ProviderAdapterRegistry::builtin();
        assert!(reg.has_protocol_name("openai-chat"));
        assert!(reg.has_protocol_name("anthropic"));
        assert!(reg.has_protocol_name("openai-responses"));
        assert!(reg.has_protocol_name("gemini"));
        assert!(!reg.has_protocol_name("unknown"));
    }

    // -- Adapter protocol method ---------------------------------------------

    #[test]
    fn adapter_protocol_dispatch() {
        let reg = ProviderAdapterRegistry::builtin();
        assert_eq!(
            reg.get(ProviderProtocol::OpenAiChatCompletions).unwrap().protocol(),
            ProviderProtocol::OpenAiChatCompletions
        );
        assert_eq!(
            reg.get(ProviderProtocol::AnthropicMessages).unwrap().protocol(),
            ProviderProtocol::AnthropicMessages
        );
        assert_eq!(
            reg.get(ProviderProtocol::OpenAiResponses).unwrap().protocol(),
            ProviderProtocol::OpenAiResponses
        );
        assert_eq!(
            reg.get(ProviderProtocol::GeminiGenerateContent).unwrap().protocol(),
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
        ).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent"
        );
    }

    #[test]
    fn url_template_no_placeholder_unchanged() {
        let target = make_target(ProviderProtocol::OpenAiChatCompletions);
        let url = expand_url_template("https://api.openai.com/v1/chat/completions", &target).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn url_template_rejects_unsafe_model_name() {
        let mut target = make_target(ProviderProtocol::OpenAiChatCompletions);
        target.upstream_model = "../../etc/passwd".into();
        let result = expand_url_template("https://api.openai.com/v1/{model}", &target);
        assert!(result.is_err(), "should reject model name with path traversal");
    }

    #[test]
    fn provider_adapter_target_debug_redacts_api_key() {
        let target = ProviderAdapterTarget {
            provider_name: "test".into(),
            adapter_name: "openai-chat".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            endpoint: "https://api.openai.com/v1/chat/completions".into(),
            auth_style: AuthStyle::Bearer,
            api_key: "sk-test-super-secret-key-1234567890".into(),
            requested_model: "gpt-4o".into(),
            upstream_model: "gpt-4o".into(),
        };
        let debug = format!("{:?}", target);
        assert!(!debug.contains("sk-test-super-secret-key-1234567890"),
            "Debug output must not contain the actual API key");
        assert!(debug.contains("***"), "Debug output must show *** for api_key");
    }

    // -- Finish reason mapping -----------------------------------------------

    #[test]
    fn openai_finish_reason_mappings() {
        assert_eq!(map_openai_finish_reason("stop"), StopReason::EndTurn);
        assert_eq!(map_openai_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_openai_finish_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_openai_finish_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_openai_finish_reason("content_filter"), StopReason::EndTurn);
        assert_eq!(map_openai_finish_reason("unknown"), StopReason::Unknown);
    }

    #[test]
    fn gemini_finish_reason_mappings() {
        assert_eq!(map_gemini_finish_reason("STOP"), StopReason::EndTurn);
        assert_eq!(map_gemini_finish_reason("MAX_TOKENS"), StopReason::MaxTokens);
        assert_eq!(map_gemini_finish_reason("SAFETY"), StopReason::EndTurn);
        assert_eq!(map_gemini_finish_reason("RECITATION"), StopReason::EndTurn);
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

    // -- Helpers --------------------------------------------------------------

    fn make_target(protocol: ProviderProtocol) -> ProviderAdapterTarget {
        let (endpoint, auth_style) = match protocol {
            ProviderProtocol::OpenAiChatCompletions => {
                ("https://api.openai.com/v1/chat/completions".into(), AuthStyle::Bearer)
            }
            ProviderProtocol::AnthropicMessages => {
                ("https://api.anthropic.com/v1/messages".into(), AuthStyle::XApiKey)
            }
            ProviderProtocol::OpenAiResponses => {
                ("https://api.openai.com/v1/responses".into(), AuthStyle::Bearer)
            }
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
        }
    }
}
