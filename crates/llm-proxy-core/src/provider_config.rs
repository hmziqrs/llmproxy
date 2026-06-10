//! TOML provider configuration types, parsing, env-var interpolation, and
//! validation for the routing system.
//!
//! This is the sole configuration system for the proxy, loaded from the main
//! `config.toml` file. The main file contains server settings; provider files
//! contain provider-scoped routing and catalog configuration.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::env_interpolate::{find_env_var_refs, find_unresolved_env_var, interpolate_env_vars};
use crate::error::CoreError;

// ---------------------------------------------------------------------------
// Main config: AppConfig
// ---------------------------------------------------------------------------

/// Top-level TOML application configuration.
///
/// Loaded from the main `config.toml`, which contains server settings only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Server bind and operational settings.
    pub server: ServerConfig,
}

// ---------------------------------------------------------------------------
// ServerConfig
// ---------------------------------------------------------------------------

/// Server operational settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP server binds to.
    pub bind: SocketAddr,
    /// Maximum request duration before timeout.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`.
    pub log_level: String,
    /// Reserved hot-reload flag. Configuration loading rejects `true`.
    pub hot_reload: bool,
    /// Per-client requests allowed per minute. `0` disables rate limiting.
    #[serde(default = "default_rate_limit_rpm")]
    pub rate_limit_rpm: u32,
    /// Whether client identity may use `X-Forwarded-For` and `X-Real-IP`.
    #[serde(default)]
    pub trust_forwarded_headers: bool,
    /// Window for rejecting identical path/body requests. `0s` disables deduplication.
    #[serde(default = "default_dedup_window")]
    #[serde(with = "humantime_serde")]
    pub dedup_window: Duration,
    /// Public server name reported in version/health endpoints.
    pub server_name: String,
}

const fn default_rate_limit_rpm() -> u32 {
    100
}

const fn default_dedup_window() -> Duration {
    Duration::ZERO
}

// ---------------------------------------------------------------------------
// ProviderFile
// ---------------------------------------------------------------------------

/// Wrapper for a single provider TOML file.
///
/// Each provider is defined in its own file with a top-level `[provider]`
/// section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFile {
    /// The provider definition.
    pub provider: ProviderConfig,
}

// ---------------------------------------------------------------------------
// ProviderConfig (custom Debug to redact api_key)
// ---------------------------------------------------------------------------

/// Provider configuration loaded from a dedicated TOML file.
///
/// The `api_key` field is redacted in [`Debug`] output and excluded from
/// [`Serialize`] output to prevent credential leakage in logs, error messages,
/// and API responses.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Provider name (e.g. `"opencode-go"`, `"opencode-zen"`).
    pub name: String,
    /// API key for authenticating with the upstream provider.
    /// Supports `${ENV_VAR}` interpolation.
    ///
    /// Redacted in Debug output. Not serialized (use explicit methods if you
    /// need to write a config file).
    ///
    /// WHY `skip_serializing`: prevents credentials from leaking through
    /// serialization paths (e.g. debug logs, API responses, config dumps).
    /// Round-tripping a `ProviderFile` through serialize/deserialize will
    /// FAIL because `skip_serializing` omits `api_key` and `api_key` is a
    /// required field. This is intentional -- credentials should never
    /// round-trip through serialization.
    #[serde(skip_serializing)]
    pub api_key: String,
    /// Authentication header style.
    pub auth_style: AuthStyle,
    /// Named adapter configurations (protocol + endpoint pairs).
    #[serde(default)]
    pub adapters: HashMap<String, ProviderAdapterConfig>,
    /// Route kind to adapter name mapping (e.g. chat_completions -> openai_adapter).
    #[serde(default)]
    pub routes: ProviderRoutesConfig,
    /// Model alias mapping (requested model -> upstream model name).
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
    /// Live model discovery configuration (optional).
    #[serde(default)]
    pub discovery: Option<ProviderDiscoveryConfig>,
    /// Model catalog configuration (optional, defaults to advisory hybrid).
    #[serde(default)]
    pub catalog: Option<ProviderCatalogConfig>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("api_key", &"[REDACTED]")
            .field("auth_style", &self.auth_style)
            .field("adapters", &self.adapters)
            .field("routes", &self.routes)
            .field("model_aliases", &self.model_aliases)
            .field("discovery", &self.discovery)
            .field("catalog", &self.catalog)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// AuthStyle
// ---------------------------------------------------------------------------

/// How the API key is sent in HTTP headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>`
    XApiKey,
    /// `x-goog-api-key: <key>` (Google/Gemini providers).
    XGoogleApiKey,
    /// Both `Authorization: Bearer <key>` and `x-api-key: <key>`.
    Both,
}

// ---------------------------------------------------------------------------
// ProviderAdapterConfig
// ---------------------------------------------------------------------------

/// A single adapter entry within a provider.
///
/// Maps a protocol name to an upstream endpoint URL.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAdapterConfig {
    /// Protocol family (e.g. `"openai_chat_completions"`, `"anthropic_messages"`).
    pub protocol: String,
    /// Upstream endpoint URL.
    pub endpoint: String,
    /// Static headers to include in upstream requests (e.g. anthropic-version).
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl std::fmt::Debug for ProviderAdapterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAdapterConfig")
            .field("protocol", &self.protocol)
            .field("endpoint", &endpoint_without_query(&self.endpoint))
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

pub(crate) fn endpoint_without_query(endpoint: &str) -> &str {
    endpoint
        .split_once('?')
        .map_or(endpoint, |(base, _query)| base)
}

// ---------------------------------------------------------------------------
// ProviderRouteKind & ProviderRoutesConfig
// ---------------------------------------------------------------------------

/// The kind of client-facing route being requested.
///
/// Each variant corresponds to an inbound endpoint that the proxy exposes.
/// Providers map these route kinds to adapter names via [`ProviderRoutesConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRouteKind {
    /// OpenAI-compatible `/chat/completions` endpoint.
    ChatCompletions,
    /// Anthropic-compatible `/messages` endpoint.
    Messages,
}

/// Maps [`ProviderRouteKind`] to adapter names within a provider.
///
/// Defined in TOML as:
///
/// ```toml
/// [provider.routes]
/// chat_completions = "openai_adapter"
/// messages = "anthropic_adapter"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRoutesConfig {
    /// Route kind: adapter name for the `chat_completions` endpoint.
    pub chat_completions: Option<String>,
    /// Route kind: adapter name for the `messages` endpoint.
    pub messages: Option<String>,
}

impl ProviderRoutesConfig {
    /// Look up the adapter name for the given route kind.
    pub fn get(&self, kind: ProviderRouteKind) -> Option<&str> {
        match kind {
            ProviderRouteKind::ChatCompletions => self.chat_completions.as_deref(),
            ProviderRouteKind::Messages => self.messages.as_deref(),
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderDiscoveryKind & ProviderDiscoveryConfig
// ---------------------------------------------------------------------------

/// Supported discovery backends for fetching upstream model lists.
///
/// Provider-specific discovery kinds are only justified when authentication,
/// pagination, or response shape differs materially from the OpenAI-compatible
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDiscoveryKind {
    /// Generic OpenAI-style `GET /v1/models` returning `{ "data": [{ "id": ... }] }`.
    #[serde(rename = "openai_compatible_models")]
    OpenAiCompatibleModels,
    /// OpenAI's own model-list endpoint (uses org-level auth).
    #[serde(rename = "openai_models")]
    OpenAiModels,
    /// Anthropic model-list endpoint (`GET /v1/models` with `x-api-key`).
    AnthropicModels,
    /// Google Gemini model-list endpoint.
    GeminiModels,
    /// Fireworks account-level model-list endpoint.
    FireworksAccountModels,
}

/// Configuration for live model discovery from an upstream provider.
///
/// Discovery does **not** run automatically during server startup. Live refresh
/// is triggered explicitly via CLI or a `refresh=live` query parameter on the
/// models endpoint.
///
/// ```toml
/// [provider.discovery]
/// kind = "openai_compatible_models"
/// endpoint = "https://api.example.com/v1/models"
/// max_pages = 100
/// max_models = 20000
/// max_response_bytes = 4194304
/// ```
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDiscoveryConfig {
    /// Which discovery backend protocol to use.
    pub kind: ProviderDiscoveryKind,
    /// Upstream URL to fetch the model list from.
    pub endpoint: String,
    /// Static headers used only for discovery requests.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Maximum number of pages fetched during one discovery refresh.
    #[serde(default = "default_discovery_max_pages")]
    pub max_pages: usize,
    /// Maximum number of model records accepted during one discovery refresh.
    #[serde(default = "default_discovery_max_models")]
    pub max_models: usize,
    /// Maximum response body size accepted for each discovery page.
    #[serde(default = "default_discovery_max_response_bytes")]
    pub max_response_bytes: usize,
}

const fn default_discovery_max_pages() -> usize {
    100
}

const fn default_discovery_max_models() -> usize {
    20_000
}

const fn default_discovery_max_response_bytes() -> usize {
    4 * 1024 * 1024
}

impl std::fmt::Debug for ProviderDiscoveryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderDiscoveryConfig")
            .field("kind", &self.kind)
            .field("endpoint", &endpoint_without_query(&self.endpoint))
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("max_pages", &self.max_pages)
            .field("max_models", &self.max_models)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ProviderCatalogMode & StaticModelCatalogEntry & ProviderCatalogConfig
// ---------------------------------------------------------------------------

/// How the model catalog is assembled for a provider.
///
/// - `Static`: only use operator-defined `[[provider.catalog.models]]` entries.
/// - `Discovered`: only use live/cached discovery results.
/// - `Hybrid`: merge both sources; static metadata wins on ID conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCatalogMode {
    /// Only use `[[provider.catalog.models]]` entries.
    Static,
    /// Only use live/cached discovery results.
    Discovered,
    /// Merge static and discovered models; static metadata wins on ID conflicts.
    #[serde(rename = "hybrid")]
    #[default]
    Hybrid,
}

/// A single static model entry in the provider catalog.
///
/// Defined in TOML as:
///
/// ```toml
/// [[provider.catalog.models]]
/// id = "accounts/fireworks/models/deepseek-v3p1"
/// display_name = "DeepSeek V3.1"
/// supports = ["chat_completions"]
/// context_length = 131_072
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticModelCatalogEntry {
    /// Model ID (must be non-empty, matches the upstream model name).
    pub id: String,
    /// Human-readable display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Which route kinds this model supports.
    #[serde(default)]
    pub supports: Vec<ProviderRouteKind>,
    /// Maximum context length in tokens, if known.
    #[serde(default)]
    pub context_length: Option<u32>,
}

/// Provider catalog configuration.
///
/// Controls how the `/providers/{provider}/v1/models` endpoint assembles its
/// response and whether catalog enforcement blocks unknown model IDs.
///
/// ```toml
/// [provider.catalog]
/// mode = "hybrid"
/// enforce = false
/// cache_ttl = "24h"
/// allow = ["*"]
/// deny = ["*-preview"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCatalogConfig {
    /// How to assemble the catalog: static, discovered, or hybrid merge.
    #[serde(default)]
    pub mode: ProviderCatalogMode,
    /// When `true`, requests using a model ID not in the filtered catalog are
    /// rejected. When `false` (default), the catalog is advisory only.
    #[serde(default)]
    pub enforce: bool,
    /// How long to cache discovered models before allowing a refresh.
    #[serde(default = "default_cache_ttl")]
    #[serde(with = "humantime_serde")]
    pub cache_ttl: Duration,
    /// Glob patterns for model IDs that are allowed. `["*"]` allows all.
    #[serde(default = "default_allow_all")]
    pub allow: Vec<String>,
    /// Glob patterns for model IDs that are denied unless a non-wildcard allow
    /// pattern explicitly includes the model.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Operator-defined static model entries.
    #[serde(default)]
    pub models: Vec<StaticModelCatalogEntry>,
}

fn default_cache_ttl() -> Duration {
    Duration::from_secs(24 * 60 * 60) // 24 hours
}

fn default_allow_all() -> Vec<String> {
    vec!["*".to_owned()]
}

impl Default for ProviderCatalogConfig {
    fn default() -> Self {
        Self {
            mode: ProviderCatalogMode::Hybrid,
            enforce: false,
            cache_ttl: default_cache_ttl(),
            allow: default_allow_all(),
            deny: Vec::new(),
            models: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Validation error
// ---------------------------------------------------------------------------

/// Errors produced during config validation.
///
/// This enum is `#[non_exhaustive]` so validation can evolve without breaking
/// downstream `match` expressions.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigValidationError {
    /// An adapter `endpoint` URL is empty.
    #[error("provider \"{provider}\": adapter \"{adapter}\" has an empty endpoint")]
    EmptyEndpoint {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
    },
    /// An environment variable referenced in `api_key` could not be resolved.
    #[error(
        "provider \"{provider}\": api_key contains unresolvable environment variable \"{var}\""
    )]
    UnresolvedEnvVar {
        /// Provider name.
        provider: String,
        /// The `${VAR}` pattern that could not be resolved.
        var: String,
    },
    /// An environment variable resolved to an empty value.
    #[error(
        "provider \"{provider}\": api_key environment variable \"{var}\" resolved to an empty value"
    )]
    EmptyEnvVar {
        /// Provider name.
        provider: String,
        /// The environment variable name.
        var: String,
    },
    /// The literal `api_key` value (after interpolation) is empty.
    #[error("provider \"{provider}\": api_key is empty")]
    EmptyApiKey {
        /// Provider name.
        provider: String,
    },
    /// A provider name is empty.
    #[error("provider name is empty")]
    EmptyProviderName,
    /// A catalog model entry has an empty ID.
    #[error("provider \"{provider}\": catalog entry has an empty id")]
    EmptyCatalogEntryId {
        /// Provider name.
        provider: String,
    },
    /// An adapter name is empty.
    #[error("provider \"{provider}\": adapter name is empty")]
    EmptyAdapterName {
        /// Provider name.
        provider: String,
    },
    /// An adapter has an empty protocol.
    #[error("provider \"{provider}\": adapter \"{adapter}\" has an empty protocol")]
    EmptyProtocol {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
    },
    /// A protocol is not in the known set supplied by the caller.
    #[error("provider \"{provider}\": adapter \"{adapter}\" uses unknown protocol \"{protocol}\"")]
    UnknownProtocol {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
        /// Protocol name.
        protocol: String,
    },
    /// An adapter endpoint has a non-HTTP(S) URL scheme.
    #[error(
        "provider \"{provider}\": adapter \"{adapter}\" has invalid endpoint scheme (expected http:// or https://): \"{endpoint}\""
    )]
    InvalidEndpointScheme {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
        /// The endpoint URL that has an invalid scheme.
        endpoint: String,
    },
    /// An adapter header name or value contains CR or LF characters.
    #[error("provider \"{provider}\": adapter \"{adapter}\" header contains CR/LF characters")]
    HeaderContainsCrlf {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
    },
    /// An adapter header uses a forbidden transport header name.
    #[error("provider \"{provider}\": adapter \"{adapter}\" sets forbidden header \"{header}\"")]
    ForbiddenHeader {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
        /// The forbidden header name.
        header: String,
    },
    /// An adapter header name is empty.
    #[error("provider \"{provider}\": adapter \"{adapter}\" has an empty header name")]
    EmptyHeaderName {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
    },
    /// A provider name contains invalid characters (must be URL-safe slug).
    #[error(
        "provider name \"{provider}\" contains invalid characters; \
         only lowercase ASCII letters, digits, hyphens, and underscores are allowed"
    )]
    InvalidProviderNameFormat {
        /// The invalid provider name.
        provider: String,
    },
    /// A route adapter name is empty.
    #[error("provider \"{provider}\": route \"{route_kind}\" has an empty adapter name")]
    EmptyRouteAdapterName {
        /// Provider name.
        provider: String,
        /// Route kind (e.g. "chat_completions" or "messages").
        route_kind: String,
    },
    /// A route references an adapter that does not exist.
    #[error(
        "provider \"{provider}\": route \"{route_kind}\" references unknown adapter \"{adapter}\""
    )]
    UnknownRouteAdapter {
        /// Provider name.
        provider: String,
        /// Route kind.
        route_kind: String,
        /// Adapter name.
        adapter: String,
    },
    /// A model alias key is empty.
    #[error("provider \"{provider}\": model alias key is empty")]
    EmptyModelAliasKey {
        /// Provider name.
        provider: String,
    },
    /// A model alias target is empty.
    #[error("provider \"{provider}\": model alias \"{alias}\" has an empty target")]
    EmptyModelAliasTarget {
        /// Provider name.
        provider: String,
        /// The alias key.
        alias: String,
    },
    /// A configured model ID cannot be safely interpolated into a Gemini URL template.
    #[error(
        "provider \"{provider}\": {location} model ID \"{model}\" contains characters that are unsafe for Gemini URL templates"
    )]
    UnsafeGeminiModelId {
        /// Provider name.
        provider: String,
        /// Configuration source, such as a catalog entry or alias target.
        location: &'static str,
        /// Unsafe model ID.
        model: String,
    },
    /// A discovery resource limit is zero.
    #[error("provider \"{provider}\": discovery limit \"{limit}\" must be greater than zero")]
    InvalidDiscoveryLimit {
        /// Provider name.
        provider: String,
        /// Invalid limit name.
        limit: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate a single [`ProviderConfig`].
///
/// Checks:
/// - provider name is non-empty (whitespace-only names are rejected)
/// - `api_key` is non-empty (after interpolation) and no unresolved `${VAR}` patterns remain
/// - all adapter names, protocol names, and endpoints are non-empty (whitespace-only values are rejected)
/// - adapter static headers reject CRLF characters and forbidden transport headers
/// - provider name is a URL-safe slug (lowercase ASCII letters, digits, hyphens, underscores)
/// - route adapter references point to existing adapters
/// - model alias keys and targets are non-empty
/// - static catalog IDs and alias targets used by Gemini URL templates are path-safe
///
/// `known_protocols` is an optional list of protocol names that are compiled
/// into the running binary. When `None`, protocol-name validation is skipped
/// (the core crate does not own the adapter registry). The server layer passes
/// `Some(ProviderAdapterRegistry::builtin().protocol_names())` for full validation.
///
/// The caller should ensure env-var interpolation has already been performed on
/// the `api_key` field before calling this function. If the raw `${VAR}` pattern
/// remains in `api_key`, it will be caught as an unresolved env var.
///
/// # Errors
///
/// Returns a [`ConfigValidationError`] variant describing the first validation
/// failure encountered. Checks run in a defined order: provider name, unresolved
/// env vars in `api_key`, empty `api_key`, adapter names/protocols/endpoints/headers,
/// protocol-name membership, route adapter references,
/// and model alias validation.
pub fn validate_provider_config(
    provider: &ProviderConfig,
    known_protocols: Option<&[&str]>,
) -> Result<(), ConfigValidationError> {
    let name = &provider.name;

    // Provider name must be non-empty (after trimming whitespace).
    if name.trim().is_empty() {
        return Err(ConfigValidationError::EmptyProviderName);
    }

    // Provider name must be a URL-safe slug.
    static PROVIDER_NAME_SLUG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[a-z0-9_-]+$").expect("valid provider name slug regex")
    });
    if !PROVIDER_NAME_SLUG.is_match(name) {
        return Err(ConfigValidationError::InvalidProviderNameFormat {
            provider: name.clone(),
        });
    }

    // api_key checks.
    if let Some(var) = find_unresolved_env_var(&provider.api_key) {
        return Err(ConfigValidationError::UnresolvedEnvVar {
            provider: name.clone(),
            var,
        });
    }
    if provider.api_key.trim().is_empty() {
        return Err(ConfigValidationError::EmptyApiKey {
            provider: name.clone(),
        });
    }

    // Adapter validation.
    for (adapter_name, adapter_cfg) in &provider.adapters {
        if adapter_name.trim().is_empty() {
            return Err(ConfigValidationError::EmptyAdapterName {
                provider: name.clone(),
            });
        }
        if adapter_cfg.protocol.trim().is_empty() {
            return Err(ConfigValidationError::EmptyProtocol {
                provider: name.clone(),
                adapter: adapter_name.clone(),
            });
        }
        if adapter_cfg.endpoint.trim().is_empty() {
            return Err(ConfigValidationError::EmptyEndpoint {
                provider: name.clone(),
                adapter: adapter_name.clone(),
            });
        }
        // Reject non-HTTP(S) URL schemes to prevent SSRF via crafted endpoints.
        // Valid schemes are "http://" and "https://". This prevents endpoints like
        // "file:///etc/passwd" or other arbitrary schemes.
        let endpoint_trimmed = adapter_cfg.endpoint.trim();
        let has_valid_scheme =
            endpoint_trimmed.starts_with("http://") || endpoint_trimmed.starts_with("https://");
        if !has_valid_scheme {
            return Err(ConfigValidationError::InvalidEndpointScheme {
                provider: name.clone(),
                adapter: adapter_name.clone(),
                endpoint: adapter_cfg.endpoint.clone(),
            });
        }
        if let Some(known) = known_protocols {
            if !known.contains(&adapter_cfg.protocol.as_str()) {
                return Err(ConfigValidationError::UnknownProtocol {
                    provider: name.clone(),
                    adapter: adapter_name.clone(),
                    protocol: adapter_cfg.protocol.clone(),
                });
            }
        }

        // Validate adapter static headers.
        for (hname, hvalue) in &adapter_cfg.headers {
            if hname.trim().is_empty() {
                return Err(ConfigValidationError::EmptyHeaderName {
                    provider: name.clone(),
                    adapter: adapter_name.clone(),
                });
            }
            if hname.contains('\r')
                || hname.contains('\n')
                || hvalue.contains('\r')
                || hvalue.contains('\n')
            {
                return Err(ConfigValidationError::HeaderContainsCrlf {
                    provider: name.clone(),
                    adapter: adapter_name.clone(),
                });
            }
            let lower = hname.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "host" | "content-length" | "transfer-encoding" | "connection"
            ) {
                return Err(ConfigValidationError::ForbiddenHeader {
                    provider: name.clone(),
                    adapter: adapter_name.clone(),
                    header: hname.clone(),
                });
            }
        }
    }

    if let Some(discovery) = &provider.discovery {
        let endpoint = discovery.endpoint.trim();
        if endpoint.is_empty() {
            return Err(ConfigValidationError::EmptyEndpoint {
                provider: name.clone(),
                adapter: "discovery".to_owned(),
            });
        }
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            return Err(ConfigValidationError::InvalidEndpointScheme {
                provider: name.clone(),
                adapter: "discovery".to_owned(),
                endpoint: discovery.endpoint.clone(),
            });
        }
        for (header_name, header_value) in &discovery.headers {
            if header_name.trim().is_empty() {
                return Err(ConfigValidationError::EmptyHeaderName {
                    provider: name.clone(),
                    adapter: "discovery".to_owned(),
                });
            }
            if header_name.contains(['\r', '\n']) || header_value.contains(['\r', '\n']) {
                return Err(ConfigValidationError::HeaderContainsCrlf {
                    provider: name.clone(),
                    adapter: "discovery".to_owned(),
                });
            }
            if matches!(
                header_name.to_ascii_lowercase().as_str(),
                "host" | "content-length" | "transfer-encoding" | "connection"
            ) {
                return Err(ConfigValidationError::ForbiddenHeader {
                    provider: name.clone(),
                    adapter: "discovery".to_owned(),
                    header: header_name.clone(),
                });
            }
        }
        for (limit, value) in [
            ("max_pages", discovery.max_pages),
            ("max_models", discovery.max_models),
            ("max_response_bytes", discovery.max_response_bytes),
        ] {
            if value == 0 {
                return Err(ConfigValidationError::InvalidDiscoveryLimit {
                    provider: name.clone(),
                    limit,
                });
            }
        }
    }

    // Catalog model entry validation.
    let uses_gemini_model_template = provider.adapters.values().any(|adapter| {
        adapter.protocol == "gemini_generate_content" && adapter.endpoint.contains("{model}")
    });
    if let Some(catalog) = &provider.catalog {
        for entry in &catalog.models {
            if entry.id.trim().is_empty() {
                return Err(ConfigValidationError::EmptyCatalogEntryId {
                    provider: name.clone(),
                });
            }
            if uses_gemini_model_template && !is_safe_gemini_model_id(&entry.id) {
                return Err(ConfigValidationError::UnsafeGeminiModelId {
                    provider: name.clone(),
                    location: "catalog entry",
                    model: entry.id.clone(),
                });
            }
        }
    }

    // Route adapter cross-reference validation.
    if let Some(ref adapter_name) = provider.routes.chat_completions {
        let aname = adapter_name.trim();
        if aname.is_empty() {
            return Err(ConfigValidationError::EmptyRouteAdapterName {
                provider: name.clone(),
                route_kind: "chat_completions".to_owned(),
            });
        }
        if !provider.adapters.contains_key(aname) {
            return Err(ConfigValidationError::UnknownRouteAdapter {
                provider: name.clone(),
                route_kind: "chat_completions".to_owned(),
                adapter: aname.to_owned(),
            });
        }
    }
    if let Some(ref adapter_name) = provider.routes.messages {
        let aname = adapter_name.trim();
        if aname.is_empty() {
            return Err(ConfigValidationError::EmptyRouteAdapterName {
                provider: name.clone(),
                route_kind: "messages".to_owned(),
            });
        }
        if !provider.adapters.contains_key(aname) {
            return Err(ConfigValidationError::UnknownRouteAdapter {
                provider: name.clone(),
                route_kind: "messages".to_owned(),
                adapter: aname.to_owned(),
            });
        }
    }

    // Model alias validation.
    for (alias_key, alias_target) in &provider.model_aliases {
        if alias_key.trim().is_empty() {
            return Err(ConfigValidationError::EmptyModelAliasKey {
                provider: name.clone(),
            });
        }
        if alias_target.trim().is_empty() {
            return Err(ConfigValidationError::EmptyModelAliasTarget {
                provider: name.clone(),
                alias: alias_key.clone(),
            });
        }
        if uses_gemini_model_template && !is_safe_gemini_model_id(alias_target) {
            return Err(ConfigValidationError::UnsafeGeminiModelId {
                provider: name.clone(),
                location: "model alias target",
                model: alias_target.clone(),
            });
        }
    }

    Ok(())
}

fn is_safe_gemini_model_id(model: &str) -> bool {
    let component = model.strip_prefix("models/").unwrap_or(model);
    !component.is_empty()
        && component != "."
        && component != ".."
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

// ---------------------------------------------------------------------------
// TOML loading
// ---------------------------------------------------------------------------

/// Load and parse a main [`AppConfig`] TOML file.
///
/// The raw file content is first processed for `${ENV_VAR}` interpolation,
/// then parsed as TOML, then validated.
///
/// # Blocking I/O
///
/// This function performs synchronous filesystem I/O and is intended for use
/// at startup. If hot-reload is implemented in future, an async variant will
/// be needed.
///
/// # Trust boundary
///
/// The `path` parameter accepts arbitrary filesystem paths without
/// canonicalisation or path-traversal checks. This is standard for
/// env-var/CLI-driven config loading -- the process owner controls the path.
/// The config file content is treated as trusted input.
///
/// # Errors
///
/// Returns a [`CoreError`] if the file cannot be read, the TOML is malformed,
/// env-var interpolation encounters issues, or route validation fails.
pub fn load_app_config(path: impl AsRef<Path>) -> Result<AppConfig, CoreError> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|source| CoreError::ConfigLoad {
        path: path.to_path_buf(),
        source,
    })?;

    // Check for env vars that resolve to empty, consistent with provider config.
    for (var_name, resolved) in find_env_var_refs(&raw) {
        if let Some(value) = resolved {
            if value.is_empty() {
                return Err(CoreError::ConfigValidation {
                    message: format!(
                        "app config environment variable \"{var_name}\" resolved to an empty value"
                    ),
                });
            }
        }
    }

    let interpolated = interpolate_env_vars(&raw);

    // After interpolation, check for any unresolved env vars (set but not
    // present in the environment). This ensures `${MISSING_VAR}` in server
    // fields like `bind` or `server_name` is caught rather than silently
    // passed through as a literal string.
    if let Some(var) = find_unresolved_env_var(&interpolated) {
        return Err(CoreError::ConfigValidation {
            message: format!("app config contains unresolvable environment variable \"{var}\""),
        });
    }

    let cfg: AppConfig = toml::from_str(&interpolated).map_err(CoreError::ConfigParse)?;
    if cfg.server.hot_reload {
        return Err(CoreError::ConfigValidation {
            message:
                "server.hot_reload=true is not supported; restart the server after config changes"
                    .to_owned(),
        });
    }
    Ok(cfg)
}

/// Load and parse a provider [`ProviderFile`] TOML file.
///
/// The raw file content is first processed for `${ENV_VAR}` interpolation,
/// then parsed as TOML, then validated.
///
/// `known_protocols` is forwarded to [`validate_provider_config`].
///
/// # Blocking I/O
///
/// This function performs synchronous filesystem I/O and is intended for use
/// at startup. If hot-reload is implemented in future, an async variant will
/// be needed.
///
/// # Trust boundary
///
/// The `path` parameter accepts arbitrary filesystem paths without
/// canonicalisation or path-traversal checks. This is standard for
/// env-var/CLI-driven config loading -- the process owner controls the path.
/// The config file content is treated as trusted input.
///
/// # Errors
///
/// Returns a [`CoreError`] if the file cannot be read, the TOML is malformed,
/// an env var referenced in the file resolves to an empty value, or validation
/// of the parsed [`ProviderConfig`] fails.
pub fn load_provider_config(
    path: impl AsRef<Path>,
    known_protocols: Option<&[&str]>,
) -> Result<ProviderConfig, CoreError> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|source| CoreError::ConfigLoad {
        path: path.to_path_buf(),
        source,
    })?;

    // Before interpolation, check if any env vars referenced in the raw TOML
    // resolve to empty strings. This must happen before interpolation replaces
    // the ${VAR} pattern, otherwise we lose the diagnostic info about which
    // env var was empty.
    //
    // WHY: We scan the raw TOML string with the shared env-var regex instead of
    // parsing the TOML twice. This avoids a redundant full TOML parse just to
    // extract the api_key value for the empty-env-var check.
    for (var_name, resolved) in find_env_var_refs(&raw) {
        match resolved {
            None => {
                // Env var is not set -- interpolation will leave ${VAR} as-is.
                // The unresolved-env-var check below (after interpolation) will
                // catch this for all fields, not just api_key.
            }
            Some(value) if value.is_empty() => {
                return Err(CoreError::ConfigValidation {
                    message: ConfigValidationError::EmptyEnvVar {
                        // Provider name is not yet known (no parse has occurred),
                        // so we include the file path as context instead.
                        provider: format!("(file: {})", path.display()),
                        var: var_name,
                    }
                    .to_string(),
                });
            }
            Some(_) => {}
        }
    }

    // Now perform full interpolation on the raw content and check for any
    // unresolved env vars that remain. This catches ${MISSING_VAR} patterns
    // in *any* field (endpoint, protocol, name), not just api_key.
    // validate_provider_config only checks api_key for unresolved patterns,
    // so a whole-file check is needed here to match load_app_config behavior.
    let interpolated = interpolate_env_vars(&raw);
    if let Some(var) = find_unresolved_env_var(&interpolated) {
        return Err(CoreError::ConfigValidation {
            message: format!(
                "provider config contains unresolvable environment variable \"{var}\""
            ),
        });
    }
    let file: ProviderFile = toml::from_str(&interpolated).map_err(CoreError::ConfigParse)?;
    validate_provider_config(&file.provider, known_protocols).map_err(|e| {
        CoreError::ConfigValidation {
            message: e.to_string(),
        }
    })?;
    Ok(file.provider)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_rejects_removed_models_table() {
        let result = toml::from_str::<AppConfig>(
            "[server]\nbind = '127.0.0.1:3456'\n[models]\nfoo = { provider = 'bar' }\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn app_config_defaults_operational_controls() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "test"
"#,
        )
        .expect("app config");

        assert_eq!(config.server.rate_limit_rpm, 100);
        assert!(!config.server.trust_forwarded_headers);
        assert_eq!(config.server.dedup_window, Duration::ZERO);
    }

    #[test]
    fn load_app_config_rejects_enabled_hot_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = true
server_name = "test"
"#,
        )
        .unwrap();

        let error = load_app_config(&path).expect_err("hot reload is unsupported");

        assert!(
            error
                .to_string()
                .contains("hot_reload=true is not supported")
        );
    }

    #[test]
    fn provider_routes_rejects_unknown_fields() {
        let result = toml::from_str::<ProviderFile>(
            r#"
[provider]
name = "example"
api_key = "key"
auth_style = "bearer"

[provider.routes]
chat_completion = "chat"
"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn provider_debug_redacts_header_values_and_endpoint_query() {
        let config: ProviderFile = toml::from_str(
            r#"
[provider]
name = "example"
api_key = "secret-api-key"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions?key=adapter-query-secret"

[provider.adapters.chat.headers]
x-secret = "adapter-secret"

[provider.routes]
chat_completions = "chat"

[provider.discovery]
kind = "openai_compatible_models"
endpoint = "https://example.com/v1/models?key=query-secret"

[provider.discovery.headers]
x-discovery-secret = "super-sensitive-value"
"#,
        )
        .expect("provider config");
        let debug = format!("{:?}", config.provider);
        assert!(!debug.contains("secret-api-key"));
        assert!(!debug.contains("adapter-secret"));
        assert!(!debug.contains("adapter-query-secret"));
        assert!(!debug.contains("super-sensitive-value"));
        assert!(!debug.contains("query-secret"));
    }

    #[test]
    fn discovery_rejects_transport_headers() {
        let mut provider: ProviderFile = toml::from_str(
            r#"
[provider]
name = "example"
api_key = "key"
auth_style = "bearer"

[provider.discovery]
kind = "openai_compatible_models"
endpoint = "https://example.com/v1/models"
"#,
        )
        .expect("provider config");
        provider
            .provider
            .discovery
            .as_mut()
            .expect("discovery")
            .headers
            .insert("host".to_owned(), "evil.example".to_owned());
        assert!(matches!(
            validate_provider_config(&provider.provider, None),
            Err(ConfigValidationError::ForbiddenHeader { .. })
        ));
    }

    #[test]
    fn discovery_limits_use_secure_defaults() {
        let provider: ProviderFile = toml::from_str(
            r#"
[provider]
name = "example"
api_key = "key"
auth_style = "bearer"

[provider.discovery]
kind = "openai_compatible_models"
endpoint = "https://example.com/v1/models"
"#,
        )
        .expect("provider config");
        let discovery = provider.provider.discovery.expect("discovery");

        assert_eq!(
            (
                discovery.max_pages,
                discovery.max_models,
                discovery.max_response_bytes,
            ),
            (100, 20_000, 4 * 1024 * 1024)
        );
    }

    #[test]
    fn discovery_rejects_zero_limits() {
        let provider: ProviderFile = toml::from_str(
            r#"
[provider]
name = "example"
api_key = "key"
auth_style = "bearer"

[provider.discovery]
kind = "openai_compatible_models"
endpoint = "https://example.com/v1/models"
max_pages = 0
"#,
        )
        .expect("provider config");

        assert!(matches!(
            validate_provider_config(&provider.provider, None),
            Err(ConfigValidationError::InvalidDiscoveryLimit {
                limit: "max_pages",
                ..
            })
        ));
    }

    #[test]
    fn gemini_alias_target_rejects_unsafe_url_template_characters() {
        let provider: ProviderFile = toml::from_str(
            r#"
[provider]
name = "gemini"
api_key = "key"
auth_style = "x-google-api-key"

[provider.adapters.generate]
protocol = "gemini_generate_content"
endpoint = "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"

[provider.routes]
messages = "generate"

[provider.model_aliases]
unsafe = "foo/../bar"
"#,
        )
        .expect("provider config");

        assert!(matches!(
            validate_provider_config(&provider.provider, None),
            Err(ConfigValidationError::UnsafeGeminiModelId {
                location: "model alias target",
                ..
            })
        ));
    }

    #[test]
    fn gemini_static_catalog_entry_rejects_unsafe_url_template_characters() {
        let provider: ProviderFile = toml::from_str(
            r#"
[provider]
name = "gemini"
api_key = "key"
auth_style = "x-google-api-key"

[provider.adapters.generate]
protocol = "gemini_generate_content"
endpoint = "https://generativelanguage.googleapis.com/v1beta/{model}:generateContent"

[provider.routes]
chat_completions = "generate"

[[provider.catalog.models]]
id = "models/group/gemini-pro"
"#,
        )
        .expect("provider config");

        assert!(matches!(
            validate_provider_config(&provider.provider, None),
            Err(ConfigValidationError::UnsafeGeminiModelId {
                location: "catalog entry",
                ..
            })
        ));
    }

    #[test]
    fn gemini_resource_model_id_is_valid_at_config_load() {
        let provider: ProviderFile = toml::from_str(
            r#"
[provider]
name = "gemini"
api_key = "key"
auth_style = "x-google-api-key"

[provider.adapters.generate]
protocol = "gemini_generate_content"
endpoint = "https://generativelanguage.googleapis.com/v1beta/{model}:generateContent"

[provider.routes]
chat_completions = "generate"

[provider.model_aliases]
pro = "models/gemini-2.5-pro"

[[provider.catalog.models]]
id = "models/gemini-2.5-pro"
"#,
        )
        .expect("provider config");

        assert!(validate_provider_config(&provider.provider, None).is_ok());
    }
}
