//! TOML provider configuration types, parsing, env-var interpolation, and
//! validation for the routing system.
//!
//! This is the sole configuration system for the proxy, loaded from the main
//! `config.toml` file. All server settings and model routing rules are defined
//! through the types in this module.

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
/// Loaded from the main `config.toml` that defines server settings and model
/// routing rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Server bind and operational settings.
    pub server: ServerConfig,
    /// Legacy model routing table (ignored under provider-based routing).
    ///
    /// Keys are client-facing model names. This field is kept for backward
    /// compatibility with existing `config.toml` files that contain a
    /// `[models]` section. Under provider-based routing (`/v1/{provider}/...`),
    /// this table is not consulted.
    #[serde(default)]
    pub models: HashMap<String, ModelRoute>,
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
    /// Whether to hot-reload config files at runtime.
    pub hot_reload: bool,
    /// Public server name reported in version/health endpoints.
    pub server_name: String,
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
    pub adapters: HashMap<String, ProviderAdapterConfig>,
    /// Provider-local model-to-adapter mapping.
    #[serde(default)]
    pub models: HashMap<String, ProviderModelConfig>,
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
            .field("models", &self.models)
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
    /// Both `Authorization: Bearer <key>` and `x-api-key: <key>`.
    Both,
}

// ---------------------------------------------------------------------------
// ProviderAdapterConfig
// ---------------------------------------------------------------------------

/// A single adapter entry within a provider.
///
/// Maps a protocol name to an upstream endpoint URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
// ProviderModelConfig
// ---------------------------------------------------------------------------

/// Maps a provider-local model name to an adapter within the same provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelConfig {
    /// Adapter name (must match a key in `ProviderConfig::adapters`).
    pub adapter: String,
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
    OpenAiCompatibleModels,
    /// OpenAI's own model-list endpoint (uses org-level auth).
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
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDiscoveryConfig {
    /// Which discovery backend protocol to use.
    pub kind: ProviderDiscoveryKind,
    /// Upstream URL to fetch the model list from.
    pub endpoint: String,
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
    /// Glob patterns for model IDs that are denied (takes precedence after allow).
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
// ModelRoute (appears in main AppConfig)
// ---------------------------------------------------------------------------

/// A routing entry that maps a client-facing model name to a provider.
///
/// Optionally specifies a different upstream model name via `upstream_model`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    /// Provider name (references a provider file by name).
    pub provider: String,
    /// Upstream model name to send to the provider. Defaults to the route key
    /// (the client-facing model name) if absent.
    pub upstream_model: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation error
// ---------------------------------------------------------------------------

/// Errors produced during config validation.
///
/// This enum is `#[non_exhaustive]` to allow adding new validation variants in
/// future phases (e.g. Phase 6 cross-file validation) without breaking changes
/// for downstream `match` expressions.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigValidationError {
    /// A provider-local model references an adapter that does not exist
    /// in the provider's `adapters` map.
    #[error("provider \"{provider}\": model \"{model}\" references unknown adapter \"{adapter}\"")]
    UnknownAdapter {
        /// Provider name.
        provider: String,
        /// Model key within the provider.
        model: String,
        /// Adapter name that was not found.
        adapter: String,
    },
    /// A provider-local model has an empty `adapter` reference.
    #[error("provider \"{provider}\": model \"{model}\" has an empty adapter reference")]
    EmptyAdapterInModel {
        /// Provider name.
        provider: String,
        /// Model key within the provider.
        model: String,
    },
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
    /// A provider-local model key is empty.
    #[error("provider \"{provider}\": model key is empty")]
    EmptyProviderModelKey {
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
/// - all provider-local model keys and their adapter references are non-empty
/// - every provider-local model points to an existing adapter
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
/// env vars in `api_key`, empty `api_key`, adapter names/protocols/endpoints,
/// model keys/adapter references, and finally protocol-name membership.
pub fn validate_provider_config(
    provider: &ProviderConfig,
    known_protocols: Option<&[&str]>,
) -> Result<(), ConfigValidationError> {
    let name = &provider.name;

    // Provider name must be non-empty (after trimming whitespace).
    if name.trim().is_empty() {
        return Err(ConfigValidationError::EmptyProviderName);
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
    }

    // Provider-local model validation.
    for (model_key, model_cfg) in &provider.models {
        if model_key.trim().is_empty() {
            return Err(ConfigValidationError::EmptyProviderModelKey {
                provider: name.clone(),
            });
        }
        if model_cfg.adapter.trim().is_empty() {
            return Err(ConfigValidationError::EmptyAdapterInModel {
                provider: name.clone(),
                model: model_key.clone(),
            });
        }
        if !provider.adapters.contains_key(&model_cfg.adapter) {
            return Err(ConfigValidationError::UnknownAdapter {
                provider: name.clone(),
                model: model_key.clone(),
                adapter: model_cfg.adapter.clone(),
            });
        }
    }

    // Catalog model entry validation.
    if let Some(catalog) = &provider.catalog {
        for entry in &catalog.models {
            if entry.id.trim().is_empty() {
                return Err(ConfigValidationError::EmptyCatalogEntryId {
                    provider: name.clone(),
                });
            }
        }
    }

    Ok(())
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
    use crate::test_support::{EnvVarGuard, TestEnvLock};
    use std::io::Write as IoWrite;

    /// Holds the mutex lock and env-var guards together so that
    /// the lock is held for the entire lifetime of the env cleanup.
    struct EnvScope {
        _lock: TestEnvLock,
        _guards: Vec<EnvVarGuard>,
    }

    /// Clean all env vars used by tests in this module.
    ///
    /// Uses the shared crate-level [`TEST_ENV_LOCK`](crate::test_support::TEST_ENV_LOCK)
    /// so that tests in `config::tests` and `provider_config::tests` are
    /// properly serialised with each other.
    fn clean_env() -> EnvScope {
        let lock = TestEnvLock::acquire();
        let guards = vec![
            EnvVarGuard::remove("LLM_PROXY_TEST_KEY"),
            EnvVarGuard::remove("LLM_PROXY_TEST_PROVIDER_KEY"),
            EnvVarGuard::remove("LLM_PROXY_EMPTY_KEY"),
            EnvVarGuard::remove("__LLM_PROXY_NEVER_SET__"),
        ];
        EnvScope {
            _lock: lock,
            _guards: guards,
        }
    }

    // -- Main TOML parse -------------------------------------------------------

    #[test]
    fn main_toml_parse() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"kimi-k2.6" = {{ provider = "opencode-go" }}
"glm-5" = {{ provider = "opencode-go" }}
"gpt-5.4" = {{ provider = "opencode-zen" }}
"claude-4" = {{ provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }}
"#
        )
        .expect("write");

        let cfg = load_app_config(&path).expect("load");
        assert_eq!(cfg.server.bind, "127.0.0.1:3456".parse().unwrap());
        assert_eq!(cfg.server.request_timeout, Duration::from_secs(300));
        assert_eq!(cfg.server.log_level, "info");
        assert!(!cfg.server.hot_reload);
        assert_eq!(cfg.server.server_name, "llm-proxy");

        assert_eq!(cfg.models.len(), 4);
        assert_eq!(cfg.models["kimi-k2.6"].provider, "opencode-go");
        assert!(cfg.models["kimi-k2.6"].upstream_model.is_none());
        assert_eq!(cfg.models["gpt-5.4"].provider, "opencode-zen");
        assert!(cfg.models["gpt-5.4"].upstream_model.is_none());
        assert_eq!(cfg.models["claude-4"].provider, "opencode-zen");
        assert_eq!(
            cfg.models["claude-4"].upstream_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    // -- Provider TOML parse ---------------------------------------------------

    #[test]
    fn provider_toml_parse() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("LLM_PROXY_TEST_PROVIDER_KEY", "sk-test-key-123");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "opencode-zen"
api_key = "${{LLM_PROXY_TEST_PROVIDER_KEY}}"
auth_style = "bearer"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://opencode.ai/zen/v1/responses"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/v1/messages"

[provider.adapters.gemini]
protocol = "gemini_generate_content"
endpoint = "https://opencode.ai/zen/v1/models/{{model}}:generateContent"

[provider.models]
"gpt-5.4" = {{ adapter = "responses" }}
"claude-sonnet-4-20250514" = {{ adapter = "anthropic" }}
"gemini-3.5-flash" = {{ adapter = "gemini" }}
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert_eq!(cfg.name, "opencode-zen");
        assert_eq!(cfg.api_key, "sk-test-key-123");
        assert!(matches!(cfg.auth_style, AuthStyle::Bearer));
        assert_eq!(cfg.adapters.len(), 3);
        assert_eq!(cfg.adapters["responses"].protocol, "openai_responses");
        assert_eq!(cfg.adapters["anthropic"].protocol, "anthropic_messages");
        assert_eq!(cfg.adapters["gemini"].protocol, "gemini_generate_content");
        assert_eq!(
            cfg.adapters["gemini"].endpoint,
            "https://opencode.ai/zen/v1/models/{model}:generateContent"
        );
        assert_eq!(cfg.models.len(), 3);
        assert_eq!(cfg.models["gpt-5.4"].adapter, "responses");
    }

    // -- ${ENV_VAR} interpolation ----------------------------------------------

    #[test]
    fn env_var_interpolation_in_provider() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("LLM_PROXY_TEST_KEY", "resolved-api-key");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "${{LLM_PROXY_TEST_KEY}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert_eq!(cfg.api_key, "resolved-api-key");
    }

    // -- Unknown env var fails validation --------------------------------------

    #[test]
    fn unknown_env_var_fails_validation() {
        let _env = clean_env();
        let _g = EnvVarGuard::remove("__LLM_PROXY_NEVER_SET__");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        // Write the TOML directly (not via write!) to avoid brace escaping confusion.
        let toml_content = r#"
[provider]
name = "test"
api_key = "${__LLM_PROXY_NEVER_SET__}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"test-model" = { adapter = "chat" }
"#;
        std::fs::write(&path, toml_content).expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("__LLM_PROXY_NEVER_SET__"),
            "expected unresolved env var error, got: {err}"
        );
    }

    // -- Empty env var fails validation ----------------------------------------

    #[test]
    fn empty_env_var_fails_validation() {
        let _env = clean_env();
        // Set the env var but to an empty string -- that should still fail.
        let _g = EnvVarGuard::set("LLM_PROXY_EMPTY_KEY", "");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "${{LLM_PROXY_EMPTY_KEY}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("resolved to an empty value"),
            "expected empty env var error, got: {err}"
        );
        assert!(
            err.contains("LLM_PROXY_EMPTY_KEY"),
            "error should mention the env var name, got: {err}"
        );
    }

    // -- Literal empty API key fails validation --------------------------------

    #[test]
    fn literal_empty_api_key_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: String::new(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("api_key is empty"),
            "expected empty api_key error, got: {err}"
        );
    }

    // -- Unknown TOML fields fail parse ----------------------------------------

    #[test]
    fn unknown_toml_fields_fail_parse() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"
unknown_field = "should_fail"

[models]
"#
        )
        .expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field"),
            "expected unknown field error, got: {err}"
        );
    }

    // -- ModelRoute with endpoint/protocol fields fails parse ------------------

    #[test]
    fn model_route_with_endpoint_fails_parse() {
        let toml_str = r#"
provider = "opencode-go"
endpoint = "https://example.com"
"#;
        let result: Result<ModelRoute, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field"),
            "expected unknown field error for endpoint, got: {err}"
        );
    }

    #[test]
    fn model_route_with_protocol_fails_parse() {
        let toml_str = r#"
provider = "opencode-go"
protocol = "openai_chat_completions"
"#;
        let result: Result<ModelRoute, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field"),
            "expected unknown field error for protocol, got: {err}"
        );
    }

    // -- Provider-local unknown adapter fails validation -----------------------

    #[test]
    fn provider_local_unknown_adapter_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "some-key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com/v1".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "model-a".to_owned(),
                    ProviderModelConfig {
                        adapter: "nonexistent".to_owned(),
                    },
                );
                m
            },
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown adapter") && err.contains("nonexistent"),
            "expected unknown adapter error, got: {err}"
        );
    }

    // -- Known protocol passes validation when supplied by caller --------------

    #[test]
    fn known_protocol_passes_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let known = vec!["openai_chat_completions", "anthropic_messages"];
        let result = validate_provider_config(&cfg, Some(&known));
        assert!(
            result.is_ok(),
            "expected validation to pass, got: {result:?}"
        );
    }

    // -- Unknown protocol fails validation when not supplied by caller ---------

    #[test]
    fn unknown_protocol_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "totally_fake_protocol".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let known = vec!["openai_chat_completions", "anthropic_messages"];
        let result = validate_provider_config(&cfg, Some(&known));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown protocol") && err.contains("totally_fake_protocol"),
            "expected unknown protocol error, got: {err}"
        );
    }

    // -- ProviderConfig Debug redacts api_key ----------------------------------

    #[test]
    fn provider_config_debug_redacts_api_key() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "super-secret-key-12345".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let debug_output = format!("{:?}", cfg);
        assert!(
            !debug_output.contains("super-secret-key-12345"),
            "Debug output must not contain the actual api_key"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should show [REDACTED] for api_key"
        );
    }

    // -- ProviderConfig api_key is not serialized ------------------------------

    #[test]
    fn provider_config_api_key_not_serialized() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "secret-key-do-not-leak".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let toml_str = toml::to_string(&cfg).expect("serialize");
        assert!(
            !toml_str.contains("secret-key-do-not-leak"),
            "api_key must not appear in serialized output"
        );
        // Verify intentional round-trip breakage: skip_serializing means the
        // serialized output does not include api_key, so deserializing it
        // without api_key fails (api_key is a required field). This is the
        // expected behavior -- credentials should never round-trip through
        // serialization.
        let file = ProviderFile { provider: cfg };
        let toml_str = toml::to_string_pretty(&file).expect("serialize");
        let result: Result<ProviderFile, _> = toml::from_str(&toml_str);
        assert!(
            result.is_err(),
            "deserialization should fail because skip_serializing omits api_key \
             and api_key is a required field, got: {result:?}"
        );
    }

    // -- deny_unknown_fields on all structs ------------------------------------

    #[test]
    fn provider_file_deny_unknown_fields() {
        let toml_str = r#"
[provider]
name = "test"
api_key = "key"
auth_style = "bearer"
bogus = true

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com"
"#;
        let result: Result<ProviderFile, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn provider_adapter_config_deny_unknown_fields() {
        let toml_str = r#"
protocol = "openai_chat_completions"
endpoint = "https://example.com"
extra = "nope"
"#;
        let result: Result<ProviderAdapterConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn provider_model_config_deny_unknown_fields() {
        let toml_str = r#"
adapter = "chat"
bonus = "nope"
"#;
        let result: Result<ProviderModelConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn server_config_deny_unknown_fields() {
        let toml_str = r#"
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"
mystery = true
"#;
        let result: Result<ServerConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn app_config_deny_unknown_fields() {
        // Put an unknown top-level field alongside server and models.
        // TOML ordering: top-level keys before section headers.
        let toml_str = r#"
unknown_top_level = "nope"

[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"#;
        let result: Result<AppConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field"),
            "expected unknown field error, got: {err}"
        );
    }

    // -- Empty endpoint validation ---------------------------------------------

    #[test]
    fn empty_endpoint_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: String::new(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty endpoint"),
            "expected empty endpoint error, got: {err}"
        );
    }

    // -- Empty provider name validation ----------------------------------------

    #[test]
    fn empty_provider_name_fails_validation() {
        let cfg = ProviderConfig {
            name: String::new(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("provider name is empty"),
            "expected empty provider name error, got: {err}"
        );
    }

    // -- Empty protocol validation ---------------------------------------------

    #[test]
    fn empty_protocol_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: String::new(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty protocol"),
            "expected empty protocol error, got: {err}"
        );
    }

    // -- Empty adapter name validation -----------------------------------------

    #[test]
    fn empty_adapter_name_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    String::new(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("adapter name is empty"),
            "expected empty adapter name error, got: {err}"
        );
    }

    // -- AuthStyle deserialization variants ------------------------------------

    #[test]
    fn auth_style_bearer() {
        let helper: AuthStyleHelper = toml::from_str(r#"auth_style = "bearer""#).expect("parse");
        assert!(matches!(helper.auth_style, AuthStyle::Bearer));
    }

    #[test]
    fn auth_style_x_api_key() {
        let helper: AuthStyleHelper = toml::from_str(r#"auth_style = "x-api-key""#).expect("parse");
        assert!(matches!(helper.auth_style, AuthStyle::XApiKey));
    }

    // Helper struct to test AuthStyle deserialization
    #[derive(Deserialize)]
    struct AuthStyleHelper {
        auth_style: AuthStyle,
    }

    #[test]
    fn auth_style_both() {
        let helper: AuthStyleHelper = toml::from_str(r#"auth_style = "both""#).expect("parse");
        assert!(matches!(helper.auth_style, AuthStyle::Both));
    }

    // -- TOML roundtrip --------------------------------------------------------

    #[test]
    fn app_config_toml_roundtrip() {
        let _env = clean_env();
        let original = AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: Duration::from_secs(300),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "llm-proxy".to_owned(),
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "kimi-k2.6".to_owned(),
                    ModelRoute {
                        provider: "opencode-go".to_owned(),
                        upstream_model: None,
                    },
                );
                m.insert(
                    "claude-4".to_owned(),
                    ModelRoute {
                        provider: "opencode-zen".to_owned(),
                        upstream_model: Some("claude-sonnet-4-20250514".to_owned()),
                    },
                );
                m
            },
        };
        let toml_str = toml::to_string_pretty(&original).expect("serialize");
        let back: AppConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(back.server.bind, original.server.bind);
        assert_eq!(back.server.request_timeout, original.server.request_timeout);
        assert_eq!(back.models.len(), 2);
        assert_eq!(
            back.models["claude-4"].upstream_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    // -- Direct test for EmptyProviderModelKey ---------------------------------

    #[test]
    fn empty_provider_model_key_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    String::new(),
                    ProviderModelConfig {
                        adapter: "chat".to_owned(),
                    },
                );
                m
            },
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("model key is empty"),
            "expected empty model key error, got: {err}"
        );
    }

    // -- Direct test for empty adapter in model config -------------------------

    #[test]
    fn empty_adapter_in_model_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "my-model".to_owned(),
                    ProviderModelConfig {
                        adapter: String::new(),
                    },
                );
                m
            },
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty adapter reference"),
            "expected empty adapter in model error, got: {err}"
        );
    }

    // -- Load nonexistent file fails -------------------------------------------

    #[test]
    fn load_app_config_nonexistent_file_fails() {
        let result = load_app_config("/tmp/__llm_proxy_no_such_file_app__.toml");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to read config"),
            "expected config load error, got: {err}"
        );
    }

    #[test]
    fn load_provider_config_nonexistent_file_fails() {
        let result = load_provider_config("/tmp/__llm_proxy_no_such_file_provider__.toml", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to read config"),
            "expected config load error, got: {err}"
        );
    }

    // -- Malformed TOML fails --------------------------------------------------

    #[test]
    fn load_app_config_malformed_toml_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml {{{}}").expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse config"),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn load_provider_config_malformed_toml_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml {{{}}").expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse config"),
            "expected parse error, got: {err}"
        );
    }

    // -- Missing required fields in TOML --------------------------------------

    #[test]
    fn load_app_config_missing_server_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[models]\n").expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse config"),
            "expected parse error for missing server, got: {err}"
        );
    }

    #[test]
    fn load_app_config_missing_bind_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"#,
        )
        .expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse config"),
            "expected parse error for missing bind, got: {err}"
        );
    }

    #[test]
    fn load_provider_config_missing_name_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
api_key = "key"
auth_style = "bearer"
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err());
    }

    // -- Wrong field types in TOML --------------------------------------------

    #[test]
    fn load_app_config_wrong_bind_type_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
bind = 3456
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"#,
        )
        .expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err());
    }

    #[test]
    fn load_app_config_wrong_hot_reload_type_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = "not_a_bool"
server_name = "llm-proxy"

[models]
"#,
        )
        .expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err());
    }

    // -- Cross-field env var interpolation (endpoint field) --------------------

    #[test]
    fn env_var_interpolation_in_endpoint() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("_LLM_PROXY_TEST_ENDPOINT", "https://custom.api.example.com");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "static-key"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "${{_LLM_PROXY_TEST_ENDPOINT}}/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert_eq!(
            cfg.adapters["chat"].endpoint,
            "https://custom.api.example.com/v1/chat/completions"
        );
    }

    // -- Provider TOML missing api_key fails at parse time ---------------------

    #[test]
    fn load_provider_config_missing_api_key_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = "test"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err(), "expected error for missing api_key");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse config"),
            "expected parse error for missing api_key, got: {err}"
        );
    }

    // -- Unknown protocol passes when known_protocols is None ------------------

    #[test]
    fn unknown_protocol_passes_when_known_protocols_is_none() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "totally_fake_protocol".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        // When known_protocols is None, protocol validation is skipped entirely.
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_ok(),
            "unknown protocol should pass when known_protocols is None, got: {result:?}"
        );
    }

    // -- Multiple env vars in a single api_key --------------------------------

    #[test]
    fn multiple_env_vars_in_api_key() {
        let _env = clean_env();
        let _g1 = EnvVarGuard::set("_LLM_PROXY_TEST_KEY_PART_A", "alpha");
        let _g2 = EnvVarGuard::set("_LLM_PROXY_TEST_KEY_PART_B", "beta");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "${{_LLM_PROXY_TEST_KEY_PART_A}}:${{_LLM_PROXY_TEST_KEY_PART_B}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert_eq!(cfg.api_key, "alpha:beta");
    }

    // -- Whitespace-only provider name fails validation -------------------------

    #[test]
    fn whitespace_only_provider_name_fails_validation() {
        // Validation uses .trim().is_empty(), so whitespace-only names are
        // rejected as semantically empty.
        let cfg = ProviderConfig {
            name: "   ".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only provider name should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("provider name is empty"),
            "expected empty provider name error, got: {err}"
        );
    }

    // -- Whitespace-only adapter name fails validation --------------------------

    #[test]
    fn whitespace_only_adapter_name_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "   ".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only adapter name should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("adapter name is empty"),
            "expected empty adapter name error, got: {err}"
        );
    }

    // -- Very long model name / endpoint is accepted ---------------------------

    #[test]
    fn very_long_endpoint_and_model_name_accepted() {
        let long_name = "a".repeat(10_000);
        let long_url = format!("https://example.com/{long_name}");
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: long_url.clone(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    long_name.clone(),
                    ProviderModelConfig {
                        adapter: "chat".to_owned(),
                    },
                );
                m
            },
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_ok(),
            "very long strings should pass validation, got: {result:?}"
        );
    }

    // -- App config with unresolved env var in server field fails --------------

    #[test]
    fn load_app_config_unresolved_env_var_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "${_LLM_PROXY_NEVER_EXISTS_FOR_APP_12345}"

[models]
"#,
        )
        .expect("write");

        let result = load_app_config(&path);
        assert!(
            result.is_err(),
            "expected error for unresolved env var in server field"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unresolvable environment variable"),
            "expected unresolved env var error, got: {err}"
        );
    }

    // -- App config with empty env var fails -----------------------------------

    #[test]
    fn load_app_config_empty_env_var_fails() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("_LLM_PROXY_APP_EMPTY_VAR", "");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "${_LLM_PROXY_APP_EMPTY_VAR}"

[models]
"#,
        )
        .expect("write");

        let result = load_app_config(&path);
        assert!(
            result.is_err(),
            "expected error for empty env var in server field"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("resolved to an empty value"),
            "expected empty env var error, got: {err}"
        );
    }

    // -- Whitespace-only whitespace-only endpoint fails validation ----------------

    #[test]
    fn whitespace_only_endpoint_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "   ".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only endpoint should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty endpoint"),
            "expected empty endpoint error, got: {err}"
        );
    }

    // -- Whitespace-only protocol fails validation --------------------------------

    #[test]
    fn whitespace_only_protocol_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "   ".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only protocol should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty protocol"),
            "expected empty protocol error, got: {err}"
        );
    }

    // -- Whitespace-only adapter reference in model fails validation ---------------

    #[test]
    fn whitespace_only_adapter_in_model_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "my-model".to_owned(),
                    ProviderModelConfig {
                        adapter: "   ".to_owned(),
                    },
                );
                m
            },
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only adapter reference should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty adapter reference"),
            "expected empty adapter reference error, got: {err}"
        );
    }

    // -- Whitespace-only api_key fails validation ---------------------------------
    //
    // api_key validation uses .trim().is_empty() consistent with all other field
    // checks, so whitespace-only api_keys are rejected.

    #[test]
    fn whitespace_only_api_key_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "   ".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only api_key should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("api_key is empty"),
            "expected empty api_key error, got: {err}"
        );
    }

    // -- Empty TOML adapter name key via file --------------------------------------

    #[test]
    fn empty_toml_adapter_name_key_via_file_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = "test"
api_key = "key"
auth_style = "bearer"

[provider.adapters.'']
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err(), "expected error for empty adapter name key");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("adapter name is empty"),
            "expected empty adapter name error, got: {err}"
        );
    }

    // -- Invalid UTF-8 in TOML file fails ------------------------------------------

    #[test]
    fn invalid_utf8_in_app_config_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        // Write invalid UTF-8 bytes (BOM-like but corrupted).
        std::fs::write(&path, b"\xff\xfe invalid utf8 content").expect("write");

        let result = load_app_config(&path);
        assert!(result.is_err(), "expected error for invalid UTF-8");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to read config"),
            "expected config load error for invalid UTF-8, got: {err}"
        );
    }

    #[test]
    fn invalid_utf8_in_provider_config_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(&path, b"\xff\xfe invalid utf8 content").expect("write");

        let result = load_provider_config(&path, None);
        assert!(result.is_err(), "expected error for invalid UTF-8");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to read config"),
            "expected config load error for invalid UTF-8, got: {err}"
        );
    }

    // -- App config with empty models map passes -----------------------------------

    #[test]
    fn load_app_config_empty_models_map_passes() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"#,
        )
        .expect("write");

        let cfg = load_app_config(&path).expect("load");
        assert!(
            cfg.models.is_empty(),
            "empty models map should parse successfully"
        );
    }

    // -- Minimal valid provider config (name + api_key + auth_style only) ----------

    #[test]
    fn minimal_valid_provider_config_passes() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_ok(),
            "minimal provider config should pass validation, got: {result:?}"
        );
    }

    // -- Duplicate env var references in same file ---------------------------------

    #[test]
    fn duplicate_env_var_references_in_same_file() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("_LLM_PROXY_DUP_VAR", "shared-value");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "${{_LLM_PROXY_DUP_VAR}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://${{_LLM_PROXY_DUP_VAR}}/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert_eq!(cfg.api_key, "shared-value");
        assert_eq!(
            cfg.adapters["chat"].endpoint,
            "https://shared-value/v1/chat/completions"
        );
    }

    // -- Env var interpolation in provider name ------------------------------------

    #[test]
    fn env_var_interpolation_in_provider_name() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("_LLM_PROXY_PROVIDER_NAME_VAR", "dynamic-provider");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "${{_LLM_PROXY_PROVIDER_NAME_VAR}}"
api_key = "static-key"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert_eq!(cfg.name, "dynamic-provider");
    }

    // -- Env var interpolation in adapter name -------------------------------------

    #[test]
    fn env_var_interpolation_in_adapter_name() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("_LLM_PROXY_ADAPTER_NAME_VAR", "dynamic-adapter");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "static-key"
auth_style = "bearer"

[provider.adapters."${{_LLM_PROXY_ADAPTER_NAME_VAR}}"]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models."test-model"]
adapter = "${{_LLM_PROXY_ADAPTER_NAME_VAR}}"
"#
        )
        .expect("write");

        let cfg = load_provider_config(&path, None).expect("load");
        assert!(cfg.adapters.contains_key("dynamic-adapter"));
        assert_eq!(cfg.models["test-model"].adapter, "dynamic-adapter");
    }

    // -- Whitespace-only provider model key fails validation -----------------------

    #[test]
    fn whitespace_only_provider_model_key_fails_validation() {
        let cfg = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                        headers: HashMap::new(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "   ".to_owned(),
                    ProviderModelConfig {
                        adapter: "chat".to_owned(),
                    },
                );
                m
            },
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };
        let result = validate_provider_config(&cfg, None);
        assert!(
            result.is_err(),
            "whitespace-only model key should fail validation, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("model key is empty"),
            "expected empty model key error, got: {err}"
        );
    }

    // -- Unresolved env var in endpoint field fails (not just api_key) -----------

    #[test]
    fn unresolved_env_var_in_endpoint_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = "test"
api_key = "static-key"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "${_LLM_PROXY_NEVER_SET_ENDPOINT_VAR}/v1/chat/completions"
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(
            result.is_err(),
            "expected error for unresolved env var in endpoint, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unresolvable environment variable"),
            "expected unresolvable env var error, got: {err}"
        );
        assert!(
            err.contains("_LLM_PROXY_NEVER_SET_ENDPOINT_VAR"),
            "error should mention the unresolved var name, got: {err}"
        );
    }

    // -- Unresolved env var in provider name fails --------------------------------

    #[test]
    fn unresolved_env_var_in_provider_name_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = "${_LLM_PROXY_NEVER_SET_NAME_VAR}"
api_key = "static-key"
auth_style = "bearer"
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(
            result.is_err(),
            "expected error for unresolved env var in provider name, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unresolvable environment variable"),
            "expected unresolvable env var error, got: {err}"
        );
    }

    // -- Unresolved env var in protocol field fails -------------------------------

    #[test]
    fn unresolved_env_var_in_protocol_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = "test"
api_key = "static-key"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "${_LLM_PROXY_NEVER_SET_PROTOCOL_VAR}"
endpoint = "https://example.com/v1/chat/completions"
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(
            result.is_err(),
            "expected error for unresolved env var in protocol, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unresolvable environment variable"),
            "expected unresolvable env var error, got: {err}"
        );
    }

    // -- Unknown AuthStyle variant produces clear error ---------------------------

    #[test]
    fn unknown_auth_style_variant_produces_clear_error() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = "test"
api_key = "key"
auth_style = "digest"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"
"#,
        )
        .expect("write");

        let result = load_provider_config(&path, None);
        assert!(
            result.is_err(),
            "expected error for unknown auth_style variant"
        );
        let err = result.unwrap_err().to_string();
        // Serde enum deserialization reports unknown variants clearly.
        assert!(
            err.contains("digest") || err.contains("unknown variant"),
            "error should mention the unknown variant, got: {err}"
        );
    }
}
