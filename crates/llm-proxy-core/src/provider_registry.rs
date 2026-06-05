//! Provider registry: load provider TOML files and resolve route targets to
//! adapter configurations.
//!
//! The registry sits between the model router ([`resolve_model_route`](crate::resolve_model_route))
//! and the provider adapter layer. The router selects *which* provider and model
//! to use; the registry resolves that selection into concrete adapter config
//! (protocol, endpoint, auth style, API key).
//!
//! # Separation of concerns
//!
//! ```text
//! resolve_model_route(...)  ->  ProviderTarget
//! ProviderRegistry::resolve_adapter_target(ProviderTarget)  ->  ProviderAdapterTargetConfig
//! ```
//!
//! The registry does **not**:
//!
//! - translate protocol fields
//! - parse response or stream chunks
//! - build final protocol-specific URL paths
//! - inspect messages or content
//! - mutate sampling, tools, metadata, reasoning, cache, or stream intent
//! - infer protocol families from model names

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::error::CoreError;
use crate::model_route::ProviderTarget;
use crate::provider_config::{AuthStyle, ProviderConfig, load_provider_config};

// ---------------------------------------------------------------------------
// ProviderAdapterTargetConfig
// ---------------------------------------------------------------------------

/// Fully resolved adapter target produced by the provider registry.
///
/// Contains everything the provider adapter layer needs to build an upstream
/// request: which provider, which adapter, which protocol, the raw endpoint
/// URL, authentication style, API key, and both the client-requested and
/// upstream model names.
///
/// `endpoint` is the raw endpoint or URL template from provider TOML. It is
/// **not** a route-built final URL. Provider adapters own endpoint URL shape,
/// including Gemini `{model}` expansion.
#[derive(Clone)]
#[non_exhaustive]
pub struct ProviderAdapterTargetConfig {
    /// Provider name (identifies the provider config).
    pub provider_name: String,
    /// Adapter name within the provider.
    pub adapter_name: String,
    /// Protocol family (e.g. `"openai_chat_completions"`, `"anthropic_messages"`).
    pub protocol: String,
    /// Raw upstream endpoint URL (may contain template placeholders like `{model}`).
    pub endpoint: String,
    /// Authentication header style.
    pub auth_style: AuthStyle,
    /// API key for the upstream provider.
    pub api_key: String,
    /// The model name the client originally requested.
    pub requested_model: String,
    /// The model name to send to the upstream provider.
    pub upstream_model: String,
}

impl std::fmt::Debug for ProviderAdapterTargetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAdapterTargetConfig")
            .field("provider_name", &self.provider_name)
            .field("adapter_name", &self.adapter_name)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field("auth_style", &self.auth_style)
            .field("api_key", &"[REDACTED]")
            .field("requested_model", &self.requested_model)
            .field("upstream_model", &self.upstream_model)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ProviderRegistry
// ---------------------------------------------------------------------------

/// Loads provider TOML files and resolves [`ProviderTarget`]s to
/// [`ProviderAdapterTargetConfig`]s.
///
/// The registry owns a map of provider configs keyed by provider name.
/// Resolution follows a strict lookup chain:
///
/// ```text
/// ProviderTarget.provider          -> provider config
/// provider.models[upstream_model]  -> adapter name
/// provider.adapters[adapter_name]  -> protocol + endpoint
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    /// Load all provider TOML files from a directory.
    ///
    /// Reads every `*.toml` file in `path`, parses each as a [`ProviderFile`],
    /// and indexes them by provider name. Returns an error if:
    ///
    /// - the directory cannot be read,
    /// - any TOML file cannot be parsed or fails validation,
    /// - two files declare the same provider name (duplicate).
    ///
    /// Protocol validation is performed separately via [`validate_protocols`]
    /// after loading, typically by passing `ProviderAdapterRegistry::protocol_names()`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ProviderResolution`] for duplicate provider names.
    /// Returns [`CoreError::ConfigLoad`], [`CoreError::ConfigParse`], or
    /// [`CoreError::ConfigValidation`] for I/O, parse, or validation failures
    /// in individual files.
    pub fn load_from_dir(
        path: impl AsRef<Path>,
    ) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let entries = std::fs::read_dir(path).map_err(|source| CoreError::ConfigLoad {
            path: path.to_path_buf(),
            source,
        })?;

        let mut providers: HashMap<String, ProviderConfig> = HashMap::new();

        // Collect and sort entries by filename for deterministic ordering across
        // platforms (filesystem order is not guaranteed).
        let mut sorted_entries: Vec<_> = entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| CoreError::ConfigLoad {
                path: path.to_path_buf(),
                source,
            })?;
        sorted_entries.sort_by_key(|e| e.file_name());

        for entry in sorted_entries {
            let file_path = entry.path();

            // Skip non-regular files (directories, pipes, sockets, etc.).
            // Note: symlinks pointing to regular files are followed (is_file()
            // returns true for them). The config directory trust boundary is
            // documented in provider_config.rs.
            if let Ok(ft) = entry.file_type() {
                if !ft.is_file() {
                    tracing::debug!(
                        path = %file_path.display(),
                        "skipping non-regular file in provider directory"
                    );
                    continue;
                }
            }

            if file_path.extension().and_then(|e| e.to_str()) != Some("toml") {
                tracing::debug!(
                    path = %file_path.display(),
                    "skipping non-toml file in provider directory"
                );
                continue;
            }

            let provider = load_provider_config(&file_path, None)?;

            if providers.contains_key(&provider.name) {
                return Err(CoreError::ProviderResolution {
                    message: format!(
                        "duplicate provider name \"{}\" in {}: provider names must be unique across all config files",
                        provider.name,
                        file_path.display()
                    ),
                });
            }

            providers.insert(provider.name.clone(), provider);
        }

        Ok(Self { providers })
    }

    /// Build a registry from an existing map of provider configs.
    ///
    /// Returns an error if any two configs share the same provider name
    /// (this should not happen if the caller built the map correctly, but
    /// we validate defensively).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ProviderResolution`] if duplicate provider names
    /// are found in the input map.
    pub fn from_providers(
        providers: impl IntoIterator<Item = ProviderConfig>,
    ) -> Result<Self, CoreError> {
        let mut map: HashMap<String, ProviderConfig> = HashMap::new();
        for provider in providers {
            if let Some(existing) = map.get(&provider.name) {
                return Err(CoreError::ProviderResolution {
                    message: format!(
                        "duplicate provider name \"{}\": defined in multiple entries",
                        existing.name
                    ),
                });
            }
            map.insert(provider.name.clone(), provider);
        }
        Ok(Self { providers: map })
    }

    /// Validate that all protocols referenced by all registered providers are
    /// in the known set.
    ///
    /// The `known_protocols` iterable should come from the compiled provider
    /// adapter registry (e.g. `ProviderAdapterRegistry::protocol_names()`),
    /// not an ad hoc string list.
    ///
    /// Accepts any iterable of items that implement `AsRef<str>`, so both
    /// `Vec<String>` and `Vec<&'static str>` (from `protocol_names()`) can be
    /// passed directly without manual conversion.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ProviderResolution`] for the first unknown protocol
    /// found, with an actionable message naming the provider, adapter, and
    /// protocol.
    pub fn validate_protocols(
        &self,
        known_protocols: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<(), CoreError> {
        // Note: This allocates a String per protocol name. The set is tiny
        // (4 built-in protocols) and this runs once at startup, so the cost
        // is negligible. A HashSet<String> is used because the input iterator
        // owns its items and we cannot borrow from them.
        let known: HashSet<String> = known_protocols
            .into_iter()
            .map(|s| s.as_ref().to_owned())
            .collect();

        for provider in self.providers.values() {
            for (adapter_name, adapter_cfg) in &provider.adapters {
                if !known.contains(&adapter_cfg.protocol) {
                    return Err(CoreError::ProviderResolution {
                        message: format!(
                            "provider \"{}\": adapter \"{}\" uses unknown protocol \"{}\"",
                            provider.name, adapter_name, adapter_cfg.protocol
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    /// Resolve a [`ProviderTarget`] to a [`ProviderAdapterTargetConfig`].
    ///
    /// Lookup chain:
    ///
    /// ```text
    /// target.provider              -> provider config
    /// provider.models[target.upstream_model] -> adapter name
    /// provider.adapters[adapter_name]        -> protocol + endpoint
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ProviderResolution`] with an actionable message if:
    ///
    /// - the provider is not in the registry,
    /// - the upstream model is not in the provider's model table,
    /// - the adapter referenced by the model is not in the provider's adapter table.
    pub fn resolve_adapter_target(
        &self,
        target: &ProviderTarget,
    ) -> Result<ProviderAdapterTargetConfig, CoreError> {
        // 1. Look up the provider.
        let provider = self.providers.get(&target.provider).ok_or_else(|| {
            CoreError::ProviderResolution {
                message: format!(
                    "unknown provider \"{}\": no provider config loaded with this name. \
                     Check that the provider TOML file exists and the name matches the route table",
                    target.provider
                ),
            }
        })?;

        // 2. Look up the provider-local model to find the adapter name.
        let model_cfg = provider.models.get(&target.upstream_model).ok_or_else(|| {
            CoreError::ProviderResolution {
                message: format!(
                    "provider \"{}\" has no model mapping for \"{}\". \
                     Add an entry in [provider.models] for this upstream model name",
                    provider.name, target.upstream_model
                ),
            }
        })?;

        // 3. Look up the adapter to get protocol and endpoint.
        let adapter_cfg = provider.adapters.get(&model_cfg.adapter).ok_or_else(|| {
            let model_key = &target.upstream_model;
            let available = if provider.adapters.is_empty() {
                "(none)".to_owned()
            } else {
                let keys: Vec<&str> = provider.adapters.keys().map(|s| s.as_str()).collect();
                if keys.len() <= 5 {
                    keys.join(", ")
                } else {
                    format!(
                        "{} (+{} more)",
                        keys[..5].join(", "),
                        keys.len() - 5
                    )
                }
            };
            CoreError::ProviderResolution {
                message: format!(
                    "provider \"{}\": model \"{}\" references adapter \"{}\" which does not exist \
                     in [provider.adapters]. Available adapters: {}",
                    provider.name,
                    model_key,
                    model_cfg.adapter,
                    available
                ),
            }
        })?;

        Ok(ProviderAdapterTargetConfig {
            provider_name: provider.name.clone(),
            adapter_name: model_cfg.adapter.clone(),
            protocol: adapter_cfg.protocol.clone(),
            endpoint: adapter_cfg.endpoint.clone(),
            auth_style: provider.auth_style,
            api_key: provider.api_key.clone(),
            requested_model: target.requested_model.clone(),
            upstream_model: target.upstream_model.clone(),
        })
    }

    /// Returns the number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Returns `true` if no providers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Returns a reference to the provider config for the given name, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_config::{ProviderAdapterConfig, ProviderModelConfig};
    use crate::test_support::{EnvVarGuard, TestEnvLock};
    use std::io::Write as IoWrite;

    /// Holds the mutex lock and env-var guards together so that
    /// the lock is held for the entire lifetime of the env cleanup.
    struct EnvScope {
        _lock: TestEnvLock,
        _guards: Vec<EnvVarGuard>,
    }

    /// Clean all env vars used by tests in this module.
    fn clean_env() -> EnvScope {
        let lock = TestEnvLock::acquire();
        let guards = vec![
            EnvVarGuard::remove("LLM_PROXY_TEST_PROVIDER_KEY"),
            EnvVarGuard::remove("LLM_PROXY_TEST_PROVIDER_KEY_B"),
            EnvVarGuard::remove("__LLM_PROXY_NEVER_SET__"),
        ];
        EnvScope {
            _lock: lock,
            _guards: guards,
        }
    }

    /// Helper: build a valid `ProviderConfig` with sensible defaults.
    fn make_provider(
        name: &str,
        adapters: HashMap<String, ProviderAdapterConfig>,
        models: HashMap<String, ProviderModelConfig>,
    ) -> ProviderConfig {
        ProviderConfig {
            name: name.to_owned(),
            api_key: "test-key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters,
            models,
        }
    }

    /// Helper: build a simple adapter config.
    fn make_adapter(protocol: &str, endpoint: &str) -> ProviderAdapterConfig {
        ProviderAdapterConfig {
            protocol: protocol.to_owned(),
            endpoint: endpoint.to_owned(),
        }
    }

    /// Helper: build a provider model config.
    fn make_model(adapter: &str) -> ProviderModelConfig {
        ProviderModelConfig {
            adapter: adapter.to_owned(),
        }
    }

    /// Helper: build a `ProviderTarget`.
    fn make_target(provider: &str, requested: &str, upstream: &str) -> ProviderTarget {
        ProviderTarget {
            provider: provider.to_owned(),
            requested_model: requested.to_owned(),
            upstream_model: upstream.to_owned(),
        }
    }

    // -- provider registry loads multiple files --------------------------------

    #[test]
    fn provider_registry_loads_multiple_files() {
        let _env = clean_env();
        let _g1 = EnvVarGuard::set("LLM_PROXY_TEST_PROVIDER_KEY", "sk-key-a");
        let _g2 = EnvVarGuard::set("LLM_PROXY_TEST_PROVIDER_KEY_B", "sk-key-b");

        let dir = tempfile::tempdir().expect("tempdir");

        // Provider A
        let path_a = dir.path().join("provider-a.toml");
        let mut f = std::fs::File::create(&path_a).expect("create");
        write!(
            f,
            r#"
[provider]
name = "provider-a"
api_key = "${{LLM_PROXY_TEST_PROVIDER_KEY}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://a.example.com/v1/chat/completions"

[provider.models]
"gpt-5" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        // Provider B
        let path_b = dir.path().join("provider-b.toml");
        let mut f = std::fs::File::create(&path_b).expect("create");
        write!(
            f,
            r#"
[provider]
name = "provider-b"
api_key = "${{LLM_PROXY_TEST_PROVIDER_KEY_B}}"
auth_style = "x-api-key"

[provider.adapters.messages]
protocol = "anthropic_messages"
endpoint = "https://b.example.com/v1/messages"

[provider.models]
"claude-sonnet-4" = {{ adapter = "messages" }}
"#
        )
        .expect("write");

        let registry = ProviderRegistry::load_from_dir(dir.path()).expect("load_from_dir");
        assert_eq!(registry.len(), 2);
        assert!(registry.get("provider-a").is_some());
        assert!(registry.get("provider-b").is_some());
    }

    // -- duplicate provider names fail -----------------------------------------

    #[test]
    fn duplicate_provider_names_fail() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("LLM_PROXY_TEST_PROVIDER_KEY", "sk-key");

        let dir = tempfile::tempdir().expect("tempdir");

        // Two files with the same provider name
        for filename in &["alpha.toml", "beta.toml"] {
            let path = dir.path().join(filename);
            let mut f = std::fs::File::create(&path).expect("create");
            write!(
                f,
                r#"
[provider]
name = "same-name"
api_key = "${{LLM_PROXY_TEST_PROVIDER_KEY}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"model-a" = {{ adapter = "chat" }}
"#
            )
            .expect("write");
        }

        let result = ProviderRegistry::load_from_dir(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("duplicate provider name"),
            "expected duplicate provider error, got: {err}"
        );
        assert!(
            err.contains("same-name"),
            "error should name the duplicate provider, got: {err}"
        );
    }

    // -- missing provider fails ------------------------------------------------

    #[test]
    fn missing_provider_fails() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "opencode-go",
            {
                let mut m = HashMap::new();
                m.insert("chat".to_owned(), make_adapter("openai_chat_completions", "https://go.example.com/v1"));
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("kimi-k2.6".to_owned(), make_model("chat"));
                m
            },
        )])
        .expect("registry");

        let target = make_target("nonexistent-provider", "kimi-k2.6", "kimi-k2.6");
        let result = registry.resolve_adapter_target(&target);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown provider") && err.contains("nonexistent-provider"),
            "expected unknown provider error, got: {err}"
        );
    }

    // -- requested model alias resolves through upstream model -----------------

    #[test]
    fn requested_model_alias_resolves_through_upstream_model() {
        let _env = clean_env();

        // Route: "claude-4" -> provider "opencode-zen", upstream "claude-sonnet-4-20250514"
        // Provider "opencode-zen": model "claude-sonnet-4-20250514" -> adapter "anthropic"
        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "opencode-zen",
            {
                let mut m = HashMap::new();
                m.insert(
                    "anthropic".to_owned(),
                    make_adapter("anthropic_messages", "https://zen.example.com/v1/messages"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert(
                    "claude-sonnet-4-20250514".to_owned(),
                    make_model("anthropic"),
                );
                m
            },
        )])
        .expect("registry");

        let target = make_target("opencode-zen", "claude-4", "claude-sonnet-4-20250514");
        let resolved = registry.resolve_adapter_target(&target).expect("resolve");
        assert_eq!(resolved.provider_name, "opencode-zen");
        assert_eq!(resolved.adapter_name, "anthropic");
        assert_eq!(resolved.protocol, "anthropic_messages");
        assert_eq!(resolved.endpoint, "https://zen.example.com/v1/messages");
        assert_eq!(resolved.requested_model, "claude-4");
        assert_eq!(resolved.upstream_model, "claude-sonnet-4-20250514");
    }

    // -- upstream_model defaults to requested_model ----------------------------

    #[test]
    fn upstream_model_defaults_to_requested_model() {
        let _env = clean_env();

        // When the route has no upstream_model, the router sets upstream_model = requested_model.
        // The registry should then look up the model using the requested name.
        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "opencode-go",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://go.example.com/v1/chat/completions"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("kimi-k2.6".to_owned(), make_model("chat"));
                m
            },
        )])
        .expect("registry");

        // Simulates: resolve_model_route returns upstream_model == requested_model
        let target = make_target("opencode-go", "kimi-k2.6", "kimi-k2.6");
        let resolved = registry.resolve_adapter_target(&target).expect("resolve");
        assert_eq!(resolved.upstream_model, "kimi-k2.6");
        assert_eq!(resolved.requested_model, "kimi-k2.6");
    }

    // -- resolved target preserves both requested and upstream model names -----

    #[test]
    fn resolved_target_preserves_both_requested_and_upstream_model_names() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "multi",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://multi.example.com/v1"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("upstream-model-x".to_owned(), make_model("chat"));
                m
            },
        )])
        .expect("registry");

        let target = make_target("multi", "my-alias", "upstream-model-x");
        let resolved = registry.resolve_adapter_target(&target).expect("resolve");
        assert_eq!(resolved.requested_model, "my-alias");
        assert_eq!(resolved.upstream_model, "upstream-model-x");
        assert_ne!(resolved.requested_model, resolved.upstream_model);
    }

    // -- provider-local missing model fails ------------------------------------

    #[test]
    fn provider_local_missing_model_fails() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "opencode-zen",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://zen.example.com/v1"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("claude-sonnet-4".to_owned(), make_model("chat"));
                m
            },
        )])
        .expect("registry");

        // Request a model that is NOT in the provider's model table
        let target = make_target("opencode-zen", "unknown-model", "unknown-model");
        let result = registry.resolve_adapter_target(&target);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no model mapping") && err.contains("unknown-model"),
            "expected missing model error, got: {err}"
        );
        assert!(
            err.contains("opencode-zen"),
            "error should name the provider, got: {err}"
        );
    }

    // -- provider-local missing adapter fails ----------------------------------

    #[test]
    fn provider_local_missing_adapter_fails() {
        let _env = clean_env();

        // Create a provider whose model references a non-existent adapter.
        // We bypass `validate_provider_config` by building the struct directly.
        let registry = ProviderRegistry::from_providers(vec![ProviderConfig {
            name: "broken".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(), // no adapters
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "some-model".to_owned(),
                    ProviderModelConfig {
                        adapter: "nonexistent-adapter".to_owned(),
                    },
                );
                m
            },
        }])
        .expect("registry");

        let target = make_target("broken", "some-model", "some-model");
        let result = registry.resolve_adapter_target(&target);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent-adapter") && err.contains("does not exist"),
            "expected missing adapter error, got: {err}"
        );
        assert!(
            err.contains("broken"),
            "error should name the provider, got: {err}"
        );
    }

    // -- protocol validation fails for unknown protocol ------------------------

    #[test]
    fn protocol_validation_fails_for_unknown_protocol() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("made_up_protocol", "https://example.com"),
                );
                m
            },
            HashMap::new(),
        )])
        .expect("registry");

        let known = vec![
            "openai_chat_completions".to_owned(),
            "anthropic_messages".to_owned(),
        ];
        let result = registry.validate_protocols(known);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown protocol") && err.contains("made_up_protocol"),
            "expected unknown protocol error, got: {err}"
        );
        assert!(
            err.contains("test") && err.contains("chat"),
            "error should name provider and adapter, got: {err}"
        );
    }

    // -- model names do not imply protocols ------------------------------------

    #[test]
    fn model_names_do_not_imply_protocols() {
        let _env = clean_env();

        // A Claude-looking model name resolves to an OpenAI adapter when TOML says so.
        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "cross-protocol",
            {
                let mut m = HashMap::new();
                m.insert(
                    "openai_adapter".to_owned(),
                    make_adapter(
                        "openai_chat_completions",
                        "https://cross.example.com/v1/chat/completions",
                    ),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert(
                    "claude-sonnet-4-20250514".to_owned(),
                    make_model("openai_adapter"),
                );
                m
            },
        )])
        .expect("registry");

        let target = make_target("cross-protocol", "claude-4", "claude-sonnet-4-20250514");
        let resolved = registry.resolve_adapter_target(&target).expect("resolve");
        assert_eq!(resolved.protocol, "openai_chat_completions");
        assert_eq!(resolved.adapter_name, "openai_adapter");
        assert_eq!(resolved.upstream_model, "claude-sonnet-4-20250514");
    }

    // -- one provider with multiple adapters resolves only by provider-local model table --

    #[test]
    fn one_provider_with_multiple_adapters_resolves_only_by_provider_local_model_table() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "multi-adapter",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter(
                        "openai_chat_completions",
                        "https://multi.example.com/v1/chat/completions",
                    ),
                );
                m.insert(
                    "messages".to_owned(),
                    make_adapter(
                        "anthropic_messages",
                        "https://multi.example.com/v1/messages",
                    ),
                );
                m.insert(
                    "gemini".to_owned(),
                    make_adapter(
                        "gemini_generate_content",
                        "https://multi.example.com/v1/models/{{model}}:generateContent",
                    ),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("gpt-5.4".to_owned(), make_model("chat"));
                m.insert("claude-sonnet-4".to_owned(), make_model("messages"));
                m.insert("gemini-3.5-flash".to_owned(), make_model("gemini"));
                m
            },
        )])
        .expect("registry");

        // Each model resolves to its own adapter
        let r1 = registry
            .resolve_adapter_target(&make_target("multi-adapter", "gpt-5.4", "gpt-5.4"))
            .expect("resolve gpt");
        assert_eq!(r1.adapter_name, "chat");
        assert_eq!(r1.protocol, "openai_chat_completions");

        let r2 = registry
            .resolve_adapter_target(&make_target("multi-adapter", "claude-4", "claude-sonnet-4"))
            .expect("resolve claude");
        assert_eq!(r2.adapter_name, "messages");
        assert_eq!(r2.protocol, "anthropic_messages");

        let r3 = registry
            .resolve_adapter_target(&make_target("multi-adapter", "gem-flash", "gemini-3.5-flash"))
            .expect("resolve gemini");
        assert_eq!(r3.adapter_name, "gemini");
        assert_eq!(r3.protocol, "gemini_generate_content");
    }

    // -- resolution errors carry actionable messages ---------------------------

    #[test]
    fn resolution_errors_carry_actionable_messages() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "provider-x",
            {
                let mut m = HashMap::new();
                m.insert(
                    "anthropic".to_owned(),
                    make_adapter("anthropic_messages", "https://x.example.com/v1/messages"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("claude-sonnet-4".to_owned(), make_model("anthropic"));
                m
            },
        )])
        .expect("registry");

        // Missing provider: names the missing provider and suggests checking files
        let err1 = registry
            .resolve_adapter_target(&make_target("no-such-provider", "m", "m"))
            .unwrap_err()
            .to_string();
        assert!(
            err1.contains("no-such-provider"),
            "missing provider error should name it, got: {err1}"
        );
        assert!(
            err1.contains("provider config"),
            "missing provider error should be actionable, got: {err1}"
        );

        // Missing model: names the provider and model, suggests adding to [provider.models]
        let err2 = registry
            .resolve_adapter_target(&make_target("provider-x", "bad-model", "bad-model"))
            .unwrap_err()
            .to_string();
        assert!(
            err2.contains("provider-x") && err2.contains("bad-model"),
            "missing model error should name provider and model, got: {err2}"
        );
        assert!(
            err2.contains("[provider.models]"),
            "missing model error should suggest fix, got: {err2}"
        );

        // Missing adapter: names provider, model, adapter, and lists available adapters
        let bad_registry = ProviderRegistry::from_providers(vec![ProviderConfig {
            name: "broken".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert("real-adapter".to_owned(), make_adapter("openai_chat_completions", "https://example.com"));
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert("my-model".to_owned(), make_model("ghost-adapter"));
                m
            },
        }])
        .expect("registry");

        let err3 = bad_registry
            .resolve_adapter_target(&make_target("broken", "my-model", "my-model"))
            .unwrap_err()
            .to_string();
        assert!(
            err3.contains("ghost-adapter") && err3.contains("does not exist"),
            "missing adapter error should name the adapter, got: {err3}"
        );
        assert!(
            err3.contains("real-adapter"),
            "missing adapter error should list available adapters, got: {err3}"
        );
    }

    // -- load_from_dir with non-existent directory fails -----------------------

    #[test]
    fn load_from_dir_nonexistent_directory_fails() {
        let _env = clean_env();
        let result = ProviderRegistry::load_from_dir("/tmp/__llm_proxy_no_such_dir__");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to read config"),
            "expected config load error for missing dir, got: {err}"
        );
    }

    // -- empty directory produces empty registry --------------------------------

    #[test]
    fn load_from_dir_empty_directory_produces_empty_registry() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load_from_dir(dir.path()).expect("load");
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    // -- directory with non-toml files ignores them ----------------------------

    #[test]
    fn load_from_dir_ignores_non_toml_files() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");

        // Write a non-toml file
        let txt_path = dir.path().join("readme.txt");
        std::fs::write(&txt_path, "this is not toml").expect("write");

        // Write a valid provider toml
        let toml_path = dir.path().join("provider.toml");
        let mut f = std::fs::File::create(&toml_path).expect("create");
        write!(
            f,
            r#"
[provider]
name = "test"
api_key = "key"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://example.com/v1/chat/completions"

[provider.models]
"test-model" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let registry = ProviderRegistry::load_from_dir(dir.path()).expect("load");
        assert_eq!(registry.len(), 1);
        assert!(registry.get("test").is_some());
    }

    // -- validate_protocols passes for all known protocols ---------------------

    #[test]
    fn validate_protocols_passes_for_all_known() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m.insert(
                    "messages".to_owned(),
                    make_adapter("anthropic_messages", "https://example.com"),
                );
                m
            },
            HashMap::new(),
        )])
        .expect("registry");

        // Use &str slices to verify AsRef<str> acceptance (matches protocol_names() return type).
        let known: Vec<&str> = vec![
            "openai_chat_completions",
            "anthropic_messages",
            "gemini_generate_content",
        ];
        let result = registry.validate_protocols(known);
        assert!(result.is_ok(), "expected validation to pass, got: {result:?}");
    }

    // -- validate_protocols passes when no protocols registered ----------------

    #[test]
    fn validate_protocols_passes_when_no_protocols_registered() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            HashMap::new(),
            HashMap::new(),
        )])
        .expect("registry");

        let known: Vec<&str> = vec!["openai_chat_completions"];
        let result = registry.validate_protocols(known);
        assert!(result.is_ok(), "expected validation to pass, got: {result:?}");
    }

    // -- validate_protocols passes with empty known set if no adapters --------

    #[test]
    fn validate_protocols_empty_known_no_adapters() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            HashMap::new(),
            HashMap::new(),
        )])
        .expect("registry");

        let result = registry.validate_protocols(Vec::<&str>::new());
        assert!(
            result.is_ok(),
            "empty known set with no adapters should pass, got: {result:?}"
        );
    }

    // -- validate_protocols fails with empty known set if adapters exist -------

    #[test]
    fn validate_protocols_empty_known_with_adapters_fails() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            HashMap::new(),
        )])
        .expect("registry");

        let result = registry.validate_protocols(Vec::<&str>::new());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown protocol"),
            "expected unknown protocol error, got: {err}"
        );
    }

    // -- validate_protocols partial mismatch fails (one valid, one unknown) ---

    #[test]
    fn validate_protocols_partial_mismatch_fails() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m.insert(
                    "weird".to_owned(),
                    make_adapter("totally_fake_protocol", "https://example.com"),
                );
                m
            },
            HashMap::new(),
        )])
        .expect("registry");

        let known: Vec<&str> = vec!["openai_chat_completions"];
        let result = registry.validate_protocols(known);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("totally_fake_protocol") && err.contains("unknown protocol"),
            "expected unknown protocol error for the mismatched adapter, got: {err}"
        );
    }

    // -- resolved config preserves auth_style and api_key ----------------------

    #[test]
    fn resolved_config_preserves_auth_style_and_api_key() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![ProviderConfig {
            name: "test".to_owned(),
            api_key: "sk-secret-key".to_owned(),
            auth_style: AuthStyle::XApiKey,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "messages".to_owned(),
                    make_adapter("anthropic_messages", "https://api.example.com/v1/messages"),
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert("claude-4".to_owned(), make_model("messages"));
                m
            },
        }])
        .expect("registry");

        let target = make_target("test", "claude-4", "claude-4");
        let resolved = registry.resolve_adapter_target(&target).expect("resolve");
        assert_eq!(resolved.auth_style, AuthStyle::XApiKey);
        assert_eq!(resolved.api_key, "sk-secret-key");
    }

    // -- ProviderAdapterTargetConfig Debug redacts api_key ---------------------

    #[test]
    fn provider_adapter_target_config_debug_redacts_api_key() {
        let config = ProviderAdapterTargetConfig {
            provider_name: "test".to_owned(),
            adapter_name: "chat".to_owned(),
            protocol: "openai_chat_completions".to_owned(),
            endpoint: "https://example.com".to_owned(),
            auth_style: AuthStyle::Bearer,
            api_key: "sk-super-secret-key-do-not-leak".to_owned(),
            requested_model: "gpt-4o".to_owned(),
            upstream_model: "gpt-4o".to_owned(),
        };
        let debug = format!("{:?}", config);
        assert!(
            !debug.contains("sk-super-secret-key-do-not-leak"),
            "Debug output must not contain the actual api_key, got: {debug}"
        );
        assert!(
            debug.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for api_key, got: {debug}"
        );
    }

    // -- from_providers with empty list produces empty registry ----------------

    #[test]
    fn from_providers_empty_produces_empty_registry() {
        let _env = clean_env();
        let registry = ProviderRegistry::from_providers(vec![]).expect("registry");
        assert!(registry.is_empty());
    }

    // -- from_providers with duplicate names fails -----------------------------

    #[test]
    fn from_providers_duplicate_names_fails() {
        let _env = clean_env();
        let p1 = make_provider("dup", HashMap::new(), HashMap::new());
        let p2 = make_provider("dup", HashMap::new(), HashMap::new());
        let result = ProviderRegistry::from_providers(vec![p1, p2]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("duplicate provider name") && err.contains("dup"),
            "expected duplicate error, got: {err}"
        );
    }

    // -- resolve_adapter_target with empty provider name yields unknown provider error --

    #[test]
    fn resolve_adapter_target_empty_provider_name_yields_unknown_provider_error() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "real-provider",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("model-a".to_owned(), make_model("chat"));
                m
            },
        )])
        .expect("registry");

        let target = make_target("", "model-a", "model-a");
        let result = registry.resolve_adapter_target(&target);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown provider") && err.contains("no provider config"),
            "expected unknown provider error for empty string, got: {err}"
        );
    }

    // -- resolve_adapter_target with empty upstream_model yields missing model error --

    #[test]
    fn resolve_adapter_target_empty_upstream_model_yields_missing_model_error() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            {
                let mut m = HashMap::new();
                m.insert("real-model".to_owned(), make_model("chat"));
                m
            },
        )])
        .expect("registry");

        let target = make_target("test", "whatever", "");
        let result = registry.resolve_adapter_target(&target);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no model mapping"),
            "expected missing model error for empty upstream_model, got: {err}"
        );
    }

    // -- load_from_dir with malformed TOML file fails --------------------------

    #[test]
    fn load_from_dir_malformed_toml_file_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let bad_path = dir.path().join("bad.toml");
        std::fs::write(&bad_path, "this is not valid toml {{{}}").expect("write");

        let result = ProviderRegistry::load_from_dir(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse config"),
            "expected parse error for malformed TOML, got: {err}"
        );
    }

    // -- load_from_dir with empty TOML file fails ------------------------------

    #[test]
    fn load_from_dir_empty_toml_file_fails() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let empty_path = dir.path().join("empty.toml");
        std::fs::write(&empty_path, "").expect("write");

        let result = ProviderRegistry::load_from_dir(dir.path());
        assert!(result.is_err(), "empty TOML file should fail validation");
    }

    // -- load_from_dir with invalid config (empty provider name) fails ---------

    #[test]
    fn load_from_dir_invalid_config_fails_validation() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("invalid.toml");
        std::fs::write(
            &path,
            r#"
[provider]
name = ""
api_key = "key"
auth_style = "bearer"

[provider.adapters]

[provider.models]
"#,
        )
        .expect("write");

        let result = ProviderRegistry::load_from_dir(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("provider name is empty"),
            "expected empty provider name error, got: {err}"
        );
    }

    // -- resolve_adapter_target with provider that has no models fails ---------

    #[test]
    fn resolve_adapter_target_provider_with_no_models_fails() {
        let _env = clean_env();

        let registry = ProviderRegistry::from_providers(vec![make_provider(
            "empty-models",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            HashMap::new(), // no models
        )])
        .expect("registry");

        let target = make_target("empty-models", "any-model", "any-model");
        let result = registry.resolve_adapter_target(&target);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no model mapping") && err.contains("any-model"),
            "expected no model mapping error, got: {err}"
        );
    }

    // -- load_from_dir sorts entries deterministically -------------------------

    #[test]
    fn load_from_dir_deterministic_ordering() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("LLM_PROXY_TEST_PROVIDER_KEY", "sk-key");

        let dir = tempfile::tempdir().expect("tempdir");

        // Write two providers in reverse alphabetical filename order.
        // The registry should still load both successfully (sorted order is stable).
        let path_b = dir.path().join("z-provider.toml");
        let mut f = std::fs::File::create(&path_b).expect("create");
        write!(
            f,
            r#"
[provider]
name = "provider-z"
api_key = "${{LLM_PROXY_TEST_PROVIDER_KEY}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://z.example.com/v1/chat/completions"

[provider.models]
"model-z" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let path_a = dir.path().join("a-provider.toml");
        let mut f = std::fs::File::create(&path_a).expect("create");
        write!(
            f,
            r#"
[provider]
name = "provider-a"
api_key = "${{LLM_PROXY_TEST_PROVIDER_KEY}}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://a.example.com/v1/chat/completions"

[provider.models]
"model-a" = {{ adapter = "chat" }}
"#
        )
        .expect("write");

        let registry = ProviderRegistry::load_from_dir(dir.path()).expect("load_from_dir");
        assert_eq!(registry.len(), 2);
        assert!(registry.get("provider-a").is_some());
        assert!(registry.get("provider-z").is_some());
    }

    // -- load_from_dir skips directories with .toml extension ------------------

    #[test]
    fn load_from_dir_skips_toml_named_directories() {
        let _env = clean_env();
        let dir = tempfile::tempdir().expect("tempdir");

        // Create a directory with .toml extension -- should be skipped.
        let toml_dir = dir.path().join("subdir.toml");
        std::fs::create_dir(&toml_dir).expect("create dir");

        let registry = ProviderRegistry::load_from_dir(dir.path()).expect("load");
        assert!(registry.is_empty(), "directory named .toml should be skipped");
    }
}
