//! TOML provider configuration types, parsing, env-var interpolation, and
//! validation for the new routing system.
//!
//! This module adds the TOML-based config that will eventually replace the old
//! JSON [`Config`](crate::Config). Both systems coexist during the migration;
//! the old config is not removed yet.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

// ---------------------------------------------------------------------------
// Env-var interpolation
// ---------------------------------------------------------------------------

/// Replace `${ENV_VAR}` patterns in `input` with the value of the
/// corresponding environment variable.  Unset variables are left as-is.
fn interpolate_env_vars(input: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\$\{([A-Za-z0-9_]+)\}").expect("env var regex is valid"));
    re.replace_all(input, |caps: &regex::Captures<'_>| {
        let var_name = &caps[1];
        std::env::var(var_name).unwrap_or_else(|_| caps[0].to_owned())
    })
    .into_owned()
}

// ---------------------------------------------------------------------------
// Main config: AppConfig
// ---------------------------------------------------------------------------

/// Top-level TOML application configuration (new system).
///
/// Replaces the old JSON [`Config`](crate::Config). Loaded from the main
/// `config.toml` that defines server settings and model routing rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Server bind and operational settings.
    pub server: ServerConfig,
    /// Model routing table. Keys are client-facing model names.
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
/// The `api_key` field is redacted in [`Debug`] output to prevent credential
/// leakage in logs, snapshots, and test failure output.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Provider name (e.g. `"opencode-go"`, `"opencode-zen"`).
    pub name: String,
    /// API key for authenticating with the upstream provider.
    /// Supports `${ENV_VAR}` interpolation.
    pub api_key: String,
    /// Authentication header style.
    pub auth_style: AuthStyle,
    /// Named adapter configurations (protocol + endpoint pairs).
    pub adapters: HashMap<String, ProviderAdapterConfig>,
    /// Provider-local model-to-adapter mapping.
    pub models: HashMap<String, ProviderModelConfig>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("api_key", &"[REDACTED]")
            .field("auth_style", &self.auth_style)
            .field("adapters", &self.adapters)
            .field("models", &self.models)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// AuthStyle
// ---------------------------------------------------------------------------

/// How the API key is sent in HTTP headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, thiserror::Error)]
pub enum ConfigValidationError {
    /// A provider-local model references an adapter that does not exist.
    #[error("provider \"{provider}\": model \"{model}\" references unknown adapter \"{adapter}\"")]
    UnknownAdapter {
        /// Provider name.
        provider: String,
        /// Model key within the provider.
        model: String,
        /// Adapter name that was not found.
        adapter: String,
    },
    /// An endpoint URL is empty.
    #[error("provider \"{provider}\": adapter \"{adapter}\" has an empty endpoint")]
    EmptyEndpoint {
        /// Provider name.
        provider: String,
        /// Adapter name.
        adapter: String,
    },
    /// An environment variable referenced in api_key could not be resolved.
    #[error("provider \"{provider}\": api_key contains unresolvable environment variable \"{var}\"")]
    UnresolvedEnvVar {
        /// Provider name.
        provider: String,
        /// The `${VAR}` pattern that could not be resolved.
        var: String,
    },
    /// An environment variable resolved to an empty value.
    #[error("provider \"{provider}\": api_key environment variable \"{var}\" resolved to an empty value")]
    EmptyEnvVar {
        /// Provider name.
        provider: String,
        /// The environment variable name.
        var: String,
    },
    /// The literal api_key value (after interpolation) is empty.
    #[error("provider \"{provider}\": api_key is empty")]
    EmptyApiKey {
        /// Provider name.
        provider: String,
    },
    /// A provider name is empty.
    #[error("provider name is empty")]
    EmptyProviderName,
    /// A route key (client-facing model name) is empty.
    #[error("model route key is empty")]
    EmptyRouteKey,
    /// A provider name in a route is empty.
    #[error("model route \"{key}\" has an empty provider")]
    EmptyRouteProvider {
        /// Route key.
        key: String,
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
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Check whether `api_key` contains any unresolved `${VAR}` patterns.
fn find_unresolved_env_var(api_key: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\$\{([A-Za-z0-9_]+)\}").expect("env var regex is valid"));
    re.captures(api_key).map(|caps| caps[1].to_owned())
}

/// Validate a single [`ProviderConfig`].
///
/// Checks:
/// - provider name is non-empty
/// - api_key is non-empty (after interpolation) and no unresolved `${VAR}` patterns remain
/// - all adapter names, protocol names, and endpoints are non-empty
/// - all provider-local model keys and their adapter references are non-empty
/// - every provider-local model points to an existing adapter
///
/// `known_protocols` is an optional list of protocol names that are compiled
/// into the running binary. When `None`, protocol-name validation is skipped
/// (the core crate does not own the adapter registry). The server layer passes
/// `Some(ProviderAdapterRegistry::builtin().protocol_names())` for full validation.
pub fn validate_provider_config(
    provider: &ProviderConfig,
    known_protocols: Option<&[&str]>,
) -> Result<(), ConfigValidationError> {
    // Provider name must be non-empty.
    if provider.name.is_empty() {
        return Err(ConfigValidationError::EmptyProviderName);
    }

    // api_key checks.
    if let Some(var) = find_unresolved_env_var(&provider.api_key) {
        return Err(ConfigValidationError::UnresolvedEnvVar {
            provider: provider.name.clone(),
            var,
        });
    }
    if provider.api_key.is_empty() {
        return Err(ConfigValidationError::EmptyApiKey {
            provider: provider.name.clone(),
        });
    }

    // Adapter validation.
    for (adapter_name, adapter_cfg) in &provider.adapters {
        if adapter_name.is_empty() {
            return Err(ConfigValidationError::EmptyAdapterName {
                provider: provider.name.clone(),
            });
        }
        if adapter_cfg.protocol.is_empty() {
            return Err(ConfigValidationError::EmptyProtocol {
                provider: provider.name.clone(),
                adapter: adapter_name.clone(),
            });
        }
        if adapter_cfg.endpoint.is_empty() {
            return Err(ConfigValidationError::EmptyEndpoint {
                provider: provider.name.clone(),
                adapter: adapter_name.clone(),
            });
        }
        if let Some(known) = known_protocols {
            if !known.contains(&adapter_cfg.protocol.as_str()) {
                return Err(ConfigValidationError::UnknownProtocol {
                    provider: provider.name.clone(),
                    adapter: adapter_name.clone(),
                    protocol: adapter_cfg.protocol.clone(),
                });
            }
        }
    }

    // Provider-local model validation.
    for (model_key, model_cfg) in &provider.models {
        if model_key.is_empty() {
            return Err(ConfigValidationError::EmptyProviderModelKey {
                provider: provider.name.clone(),
            });
        }
        if model_cfg.adapter.is_empty() {
            return Err(ConfigValidationError::UnknownAdapter {
                provider: provider.name.clone(),
                model: model_key.clone(),
                adapter: String::new(),
            });
        }
        if !provider.adapters.contains_key(&model_cfg.adapter) {
            return Err(ConfigValidationError::UnknownAdapter {
                provider: provider.name.clone(),
                model: model_key.clone(),
                adapter: model_cfg.adapter.clone(),
            });
        }
    }

    Ok(())
}

/// Validate the model routing table in [`AppConfig`].
///
/// Checks that route keys and provider names are non-empty.
pub fn validate_model_routes(models: &HashMap<String, ModelRoute>) -> Result<(), ConfigValidationError> {
    for (key, route) in models {
        if key.is_empty() {
            return Err(ConfigValidationError::EmptyRouteKey);
        }
        if route.provider.is_empty() {
            return Err(ConfigValidationError::EmptyRouteProvider {
                key: key.clone(),
            });
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
pub fn load_app_config(path: impl AsRef<Path>) -> Result<AppConfig, CoreError> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|source| CoreError::ConfigLoad {
        path: path.to_path_buf(),
        source,
    })?;
    let interpolated = interpolate_env_vars(&raw);
    let cfg: AppConfig = toml::from_str(&interpolated).map_err(CoreError::ConfigParse)?;
    validate_model_routes(&cfg.models).map_err(|e| CoreError::ConfigValidation {
        message: e.to_string(),
    })?;
    Ok(cfg)
}

/// Load and parse a provider [`ProviderFile`] TOML file.
///
/// The raw file content is first processed for `${ENV_VAR}` interpolation,
/// then parsed as TOML, then validated.
///
/// `known_protocols` is forwarded to [`validate_provider_config`].
pub fn load_provider_config(
    path: impl AsRef<Path>,
    known_protocols: Option<&[&str]>,
) -> Result<ProviderConfig, CoreError> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|source| CoreError::ConfigLoad {
        path: path.to_path_buf(),
        source,
    })?;
    let interpolated = interpolate_env_vars(&raw);
    let file: ProviderFile = toml::from_str(&interpolated).map_err(CoreError::ConfigParse)?;
    validate_provider_config(&file.provider, known_protocols).map_err(|e| CoreError::ConfigValidation {
        message: e.to_string(),
    })?;
    Ok(file.provider)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use std::sync::Mutex;

    /// Serialize all tests that touch environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that saves and restores an environment variable.
    struct EnvVarGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_owned(),
                original,
            }
        }

        fn remove(key: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self {
                key: key.to_owned(),
                original,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => unsafe {
                    std::env::set_var(&self.key, val);
                },
                None => unsafe {
                    std::env::remove_var(&self.key);
                },
            }
        }
    }

    struct EnvScope {
        _lock: std::sync::MutexGuard<'static, ()>,
        _guards: Vec<EnvVarGuard>,
    }

    fn clean_env() -> EnvScope {
        // Recover from a poisoned mutex caused by a previous test panic,
        // so that one failure does not cascade to every other env-touching test.
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
endpoint = "https://opencode.ai/zen/v1/models/{{{{model}}}}:generateContent"

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
        // Set the env var but to an empty string — that should still fail.
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
            err.contains("api_key is empty"),
            "expected empty api_key error, got: {err}"
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
                    },
                );
                m
            },
            models: HashMap::new(),
        };
        let known = vec!["openai_chat_completions", "anthropic_messages"];
        let result = validate_provider_config(&cfg, Some(&known));
        assert!(result.is_ok(), "expected validation to pass, got: {result:?}");
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
                    },
                );
                m
            },
            models: HashMap::new(),
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
                    },
                );
                m
            },
            models: HashMap::new(),
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
                    },
                );
                m
            },
            models: HashMap::new(),
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
                    },
                );
                m
            },
            models: HashMap::new(),
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
        assert_eq!(back.models["claude-4"].upstream_model.as_deref(), Some("claude-sonnet-4-20250514"));
    }
}
