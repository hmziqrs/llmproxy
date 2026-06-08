//! Provider registry: load provider TOML files and resolve route targets to
//! adapter configurations.
//!
//! # Provider-based routing
//!
//! ```text
//! provider_name + route_kind  -> provider config + route -> adapter name
//! adapter_name                -> protocol + endpoint + headers
//! requested_model             -> model_aliases -> upstream_model
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
use crate::provider_config::{
    AuthStyle, ProviderConfig, ProviderRouteKind, StaticModelCatalogEntry, load_provider_config,
};

// ---------------------------------------------------------------------------
// ProviderAdapterTargetConfig
// ---------------------------------------------------------------------------

/// Fully resolved adapter target produced by the provider registry.
///
/// Contains everything the provider adapter layer needs to build an upstream
/// request: which provider, which adapter, which protocol, the raw endpoint
/// URL, authentication style, API key, optional static headers, and both the
/// client-requested and upstream model names.
///
/// `endpoint` is the raw endpoint or URL template from provider TOML. It is
/// **not** a route-built final URL. Provider adapters own endpoint URL shape,
/// including Gemini `{model}` expansion.
#[derive(Clone, PartialEq)]
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
    /// Optional static headers from the adapter config (e.g. `anthropic-version`).
    pub headers: HashMap<String, String>,
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
            .field("headers", &self.headers)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ProviderRegistry
// ---------------------------------------------------------------------------

/// Loads provider TOML files and resolves provider routes to
/// [`ProviderAdapterTargetConfig`]s.
///
/// The registry owns a map of provider configs keyed by provider name.
/// Resolution follows a strict lookup chain:
///
/// ```text
/// provider_name                    -> provider config
/// provider.routes[route_kind]     -> adapter name
/// provider.adapters[adapter_name] -> protocol + endpoint + headers
/// provider.model_aliases[model]   -> upstream_model (if present)
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    /// Load all provider TOML files from a directory.
    #[must_use = "load_from_dir returns a Result that must be checked"]
    ///
    /// Reads every `*.toml` file in `path`, parses each as a [`crate::provider_config::ProviderFile`],
    /// and indexes them by provider name. Returns an error if:
    ///
    /// - the directory cannot be read,
    /// - any TOML file cannot be parsed or fails validation,
    /// - two files declare the same provider name (duplicate).
    ///
    /// Protocol validation is performed separately via [`Self::validate_protocols`]
    /// after loading, typically by passing `ProviderAdapterRegistry::protocol_names()`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ProviderResolution`] for duplicate provider names.
    /// Returns [`CoreError::ConfigLoad`], [`CoreError::ConfigParse`], or
    /// [`CoreError::ConfigValidation`] for I/O, parse, or validation failures
    /// in individual files.
    pub fn load_from_dir(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let entries = std::fs::read_dir(path).map_err(|source| CoreError::ConfigLoad {
            path: path.to_path_buf(),
            source,
        })?;

        let mut providers: HashMap<String, ProviderConfig> = HashMap::new();
        // Track which file each provider name was first loaded from, so that
        // duplicate-name errors can report *both* file paths for quick diagnosis.
        let mut source_paths: HashMap<String, std::path::PathBuf> = HashMap::new();

        // Collect and sort entries by filename for deterministic ordering across
        // platforms (filesystem order is not guaranteed).
        let mut sorted_entries: Vec<_> =
            entries
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| CoreError::ConfigLoad {
                    path: path.to_path_buf(),
                    source,
                })?;
        sorted_entries.sort_by_key(|e| e.file_name());

        for entry in sorted_entries {
            let file_path = entry.path();

            // Skip symlinks to prevent a malicious actor with write access to
            // the providers directory from tricking the proxy into reading
            // arbitrary files. This check uses symlink_metadata which does not
            // follow symlinks, unlike entry.file_type() which does.
            if file_path.is_symlink() {
                tracing::warn!(
                    path = %file_path.display(),
                    "skipping symlink in provider directory (security: only regular files are loaded)"
                );
                continue;
            }

            // Skip non-regular files (directories, pipes, sockets, etc.).
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
                let first_path = source_paths
                    .get(&provider.name)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(unknown)".to_owned());
                return Err(CoreError::ProviderResolution {
                    message: format!(
                        "duplicate provider name \"{}\": already defined in {}, redefined in {}. \
                         Provider names must be unique across all config files",
                        provider.name,
                        first_path,
                        file_path.display()
                    ),
                });
            }

            source_paths.insert(provider.name.clone(), file_path.clone());
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
    #[must_use = "from_providers returns a Result that must be checked"]
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

    /// Resolve a provider route by provider name, route kind, and requested model.
    ///
    /// Lookup chain:
    ///
    /// ```text
    /// provider_name                         -> provider config
    /// provider.routes[route_kind]           -> adapter name
    /// provider.adapters[adapter_name]       -> protocol + endpoint + headers
    /// provider.model_aliases[requested_model] -> upstream_model (if present)
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ProviderResolution`] with an actionable message if:
    ///
    /// - the provider is not in the registry,
    /// - the route kind is not configured for this provider,
    /// - the adapter referenced by the route is not in the provider's adapter table.
    pub fn resolve_provider_route(
        &self,
        provider_name: &str,
        route_kind: ProviderRouteKind,
        requested_model: &str,
    ) -> Result<ProviderAdapterTargetConfig, CoreError> {
        // 1. Look up the provider.
        let provider = self.providers.get(provider_name).ok_or_else(|| {
            CoreError::ProviderResolution {
                message: format!(
                    "unknown provider \"{provider_name}\": no provider config loaded with this name"
                ),
            }
        })?;

        // 2. Look up the route to find the adapter name.
        let adapter_name = provider.routes.get(route_kind).ok_or_else(|| {
            let route_name = match route_kind {
                ProviderRouteKind::ChatCompletions => "chat_completions",
                ProviderRouteKind::Messages => "messages",
            };
            CoreError::ProviderResolution {
                message: format!(
                    "provider \"{}\" does not support route \"{}\". \
                     Add a [provider.routes.{}] entry mapping to an adapter name",
                    provider.name, route_name, route_name
                ),
            }
        })?;

        // 3. Look up the adapter to get protocol, endpoint, and headers.
        let adapter_cfg = provider.adapters.get(adapter_name).ok_or_else(|| {
            let available = if provider.adapters.is_empty() {
                "(none)".to_owned()
            } else {
                let keys: Vec<&str> = provider.adapters.keys().map(|s| s.as_str()).collect();
                if keys.len() <= 5 {
                    keys.join(", ")
                } else {
                    format!("{} (+{} more)", keys[..5].join(", "), keys.len() - 5)
                }
            };
            CoreError::ProviderResolution {
                message: format!(
                    "provider \"{}\": route references adapter \"{}\" which does not exist \
                     in [provider.adapters]. Available adapters: {}",
                    provider.name, adapter_name, available
                ),
            }
        })?;

        // 4. Apply provider-local model alias if present.
        let upstream_model = provider
            .model_aliases
            .get(requested_model)
            .map(|s| s.as_str())
            .unwrap_or(requested_model);

        Ok(ProviderAdapterTargetConfig {
            provider_name: provider.name.clone(),
            adapter_name: adapter_name.to_owned(),
            protocol: adapter_cfg.protocol.clone(),
            endpoint: adapter_cfg.endpoint.clone(),
            auth_style: provider.auth_style,
            api_key: provider.api_key.clone(),
            requested_model: requested_model.to_owned(),
            upstream_model: upstream_model.to_owned(),
            headers: adapter_cfg.headers.clone(),
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

    /// Returns an iterator over all registered provider configs.
    pub fn iter(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.values()
    }

    /// Returns the static catalog entries for a provider, if a catalog is configured.
    ///
    /// Returns `None` if the provider is not found. Returns `Some(&[])` if the
    /// provider exists but has no catalog configured (or an empty static list).
    #[must_use]
    pub fn catalog_models(&self, provider_name: &str) -> Option<&[StaticModelCatalogEntry]> {
        self.providers.get(provider_name).map(|p| match &p.catalog {
            Some(catalog) => catalog.models.as_slice(),
            None => &[],
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_config::{
        ProviderAdapterConfig, ProviderModelConfig, ProviderRoutesConfig,
    };
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
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        }
    }

    /// Helper: build a provider config with routes configured for provider-based routing.
    fn make_routed_provider(
        name: &str,
        adapters: HashMap<String, ProviderAdapterConfig>,
        routes: ProviderRoutesConfig,
        model_aliases: HashMap<String, String>,
    ) -> ProviderConfig {
        ProviderConfig {
            name: name.to_owned(),
            api_key: "test-key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters,
            models: HashMap::new(),
            routes,
            model_aliases,
            discovery: None,
            catalog: None,
        }
    }

    /// Helper: build a simple adapter config.
    fn make_adapter(protocol: &str, endpoint: &str) -> ProviderAdapterConfig {
        ProviderAdapterConfig {
            protocol: protocol.to_owned(),
            endpoint: endpoint.to_owned(),
            headers: HashMap::new(),
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
        // The error should report both files so the user can find the collision.
        assert!(
            err.contains("alpha.toml") && err.contains("beta.toml"),
            "error should name both files, got: {err}"
        );
    }

    // -- resolve_provider_route: missing provider fails -------------------------

    #[test]
    fn resolve_provider_route_missing_provider_fails() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "opencode-go",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://go.example.com/v1"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let result = registry.resolve_provider_route(
            "nonexistent-provider",
            ProviderRouteKind::ChatCompletions,
            "kimi-k2.6",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown provider") && err.contains("nonexistent-provider"),
            "expected unknown provider error, got: {err}"
        );
    }

    // -- resolve_provider_route: model alias rewrites upstream model -----------

    #[test]
    fn resolve_provider_route_model_alias_rewrites_upstream() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: None,
            messages: Some("anthropic".to_owned()),
        };

        let mut aliases = HashMap::new();
        aliases.insert("claude-4".to_owned(), "claude-sonnet-4-20250514".to_owned());

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "opencode-zen",
            {
                let mut m = HashMap::new();
                m.insert(
                    "anthropic".to_owned(),
                    make_adapter("anthropic_messages", "https://zen.example.com/v1/messages"),
                );
                m
            },
            routes,
            aliases,
        )])
        .expect("registry");

        let resolved = registry
            .resolve_provider_route("opencode-zen", ProviderRouteKind::Messages, "claude-4")
            .expect("resolve");
        assert_eq!(resolved.provider_name, "opencode-zen");
        assert_eq!(resolved.adapter_name, "anthropic");
        assert_eq!(resolved.protocol, "anthropic_messages");
        assert_eq!(resolved.requested_model, "claude-4");
        assert_eq!(resolved.upstream_model, "claude-sonnet-4-20250514");
    }

    // -- resolve_provider_route: no alias means requested == upstream ----------

    #[test]
    fn resolve_provider_route_no_alias_passes_model_through() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "opencode-go",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter(
                        "openai_chat_completions",
                        "https://go.example.com/v1/chat/completions",
                    ),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let resolved = registry
            .resolve_provider_route(
                "opencode-go",
                ProviderRouteKind::ChatCompletions,
                "kimi-k2.6",
            )
            .expect("resolve");
        assert_eq!(resolved.requested_model, "kimi-k2.6");
        assert_eq!(resolved.upstream_model, "kimi-k2.6");
    }

    // -- resolve_provider_route: unsupported route kind fails -------------------

    #[test]
    fn resolve_provider_route_unsupported_route_kind_fails() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig::default(); // no routes configured

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "opencode-zen",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://zen.example.com/v1"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let result = registry.resolve_provider_route(
            "opencode-zen",
            ProviderRouteKind::ChatCompletions,
            "some-model",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not support route") && err.contains("opencode-zen"),
            "expected unsupported route error, got: {err}"
        );
    }

    // -- resolve_provider_route: missing adapter in route fails ----------------

    #[test]
    fn resolve_provider_route_missing_adapter_fails() {
        let _env = clean_env();

        // Bypass validation by building a ProviderConfig whose route references
        // a non-existent adapter.
        let routes = ProviderRoutesConfig {
            chat_completions: Some("nonexistent-adapter".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![ProviderConfig {
            name: "broken".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
            routes,
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        }])
        .expect("registry");

        let result = registry.resolve_provider_route(
            "broken",
            ProviderRouteKind::ChatCompletions,
            "some-model",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent-adapter") && err.contains("does not exist"),
            "expected missing adapter error, got: {err}"
        );
    }

    // -- resolve_provider_route: multi-adapter provider resolves per route -----

    #[test]
    fn resolve_provider_route_multi_adapter_resolves_per_route() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: Some("messages".to_owned()),
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
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
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let r1 = registry
            .resolve_provider_route(
                "multi-adapter",
                ProviderRouteKind::ChatCompletions,
                "gpt-5.4",
            )
            .expect("resolve chat");
        assert_eq!(r1.adapter_name, "chat");
        assert_eq!(r1.protocol, "openai_chat_completions");
        assert_eq!(r1.upstream_model, "gpt-5.4");

        let r2 = registry
            .resolve_provider_route(
                "multi-adapter",
                ProviderRouteKind::Messages,
                "claude-sonnet-4",
            )
            .expect("resolve messages");
        assert_eq!(r2.adapter_name, "messages");
        assert_eq!(r2.protocol, "anthropic_messages");
        assert_eq!(r2.upstream_model, "claude-sonnet-4");
    }

    // -- resolve_provider_route: preserves auth_style and api_key --------------

    #[test]
    fn resolve_provider_route_preserves_auth_style_and_api_key() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: None,
            messages: Some("messages".to_owned()),
        };

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
            models: HashMap::new(),
            routes,
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        }])
        .expect("registry");

        let resolved = registry
            .resolve_provider_route("test", ProviderRouteKind::Messages, "claude-4")
            .expect("resolve");
        assert_eq!(resolved.auth_style, AuthStyle::XApiKey);
        assert_eq!(resolved.api_key, "sk-secret-key");
    }

    // -- resolve_provider_route: resolution errors carry actionable messages ---

    #[test]
    fn resolve_provider_route_errors_are_actionable() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "provider-x",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://x.example.com/v1"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        // Missing provider: names the missing provider
        let err1 = registry
            .resolve_provider_route("no-such-provider", ProviderRouteKind::ChatCompletions, "m")
            .unwrap_err()
            .to_string();
        assert!(
            err1.contains("no-such-provider"),
            "missing provider error should name it, got: {err1}"
        );

        // Unsupported route: names the provider and suggests adding route config
        let err2 = registry
            .resolve_provider_route("provider-x", ProviderRouteKind::Messages, "m")
            .unwrap_err()
            .to_string();
        assert!(
            err2.contains("provider-x") && err2.contains("does not support route"),
            "unsupported route error should be actionable, got: {err2}"
        );
    }

    // -- resolve_provider_route: empty provider name yields unknown provider ---

    #[test]
    fn resolve_provider_route_empty_provider_name_yields_unknown_provider_error() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "real-provider",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let result =
            registry.resolve_provider_route("", ProviderRouteKind::ChatCompletions, "model-a");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown provider"),
            "expected unknown provider error for empty string, got: {err}"
        );
    }

    // -- resolve_provider_route: unicode names resolve correctly ----------------

    #[test]
    fn resolve_provider_route_unicode_names_resolve() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "プロバイダー",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com/v1"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let resolved = registry
            .resolve_provider_route(
                "プロバイダー",
                ProviderRouteKind::ChatCompletions,
                "モデル-🤖",
            )
            .expect("resolve");
        assert_eq!(resolved.provider_name, "プロバイダー");
        assert_eq!(resolved.upstream_model, "モデル-🤖");
    }

    // -- resolve_provider_route: very long model name passes through ------------

    #[test]
    fn resolve_provider_route_very_long_model_name() {
        let _env = clean_env();

        let long_name = "a".repeat(10_000);

        let routes = ProviderRoutesConfig {
            chat_completions: Some("chat".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let resolved = registry
            .resolve_provider_route("test", ProviderRouteKind::ChatCompletions, &long_name)
            .expect("resolve");
        assert_eq!(resolved.upstream_model, long_name);
    }

    // -- resolve_provider_route: special characters in adapter name work -------

    #[test]
    fn resolve_provider_route_special_characters_in_adapter_name() {
        let _env = clean_env();

        let routes = ProviderRoutesConfig {
            chat_completions: Some("a.dapt/er".to_owned()),
            messages: None,
        };

        let registry = ProviderRegistry::from_providers(vec![make_routed_provider(
            "test",
            {
                let mut m = HashMap::new();
                m.insert(
                    "a.dapt/er".to_owned(),
                    make_adapter("openai_chat_completions", "https://example.com"),
                );
                m
            },
            routes,
            HashMap::new(),
        )])
        .expect("registry");

        let resolved = registry
            .resolve_provider_route(
                "test",
                ProviderRouteKind::ChatCompletions,
                "model.with.dots/and:colons",
            )
            .expect("resolve");
        assert_eq!(resolved.adapter_name, "a.dapt/er");
        assert_eq!(resolved.upstream_model, "model.with.dots/and:colons");
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
        assert!(
            result.is_ok(),
            "expected validation to pass, got: {result:?}"
        );
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
        assert!(
            result.is_ok(),
            "expected validation to pass, got: {result:?}"
        );
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
            headers: HashMap::new(),
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

    // -- load_from_dir sorts entries deterministically -------------------------

    #[test]
    fn load_from_dir_deterministic_ordering() {
        let _env = clean_env();
        let _g = EnvVarGuard::set("LLM_PROXY_TEST_PROVIDER_KEY", "sk-key");

        let dir = tempfile::tempdir().expect("tempdir");

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

        let toml_dir = dir.path().join("subdir.toml");
        std::fs::create_dir(&toml_dir).expect("create dir");

        let registry = ProviderRegistry::load_from_dir(dir.path()).expect("load");
        assert!(
            registry.is_empty(),
            "directory named .toml should be skipped"
        );
    }

    // -- Send+Sync static assertion for ProviderRegistry -------------------------

    #[test]
    fn provider_registry_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<ProviderRegistry>();
        check::<ProviderAdapterTargetConfig>();
    }

    // -- ProviderAdapterTargetConfig Clone round-trip ----------------------------

    #[test]
    fn provider_adapter_target_config_clone_round_trip() {
        let original = ProviderAdapterTargetConfig {
            provider_name: "test".to_owned(),
            adapter_name: "chat".to_owned(),
            protocol: "openai_chat_completions".to_owned(),
            endpoint: "https://example.com".to_owned(),
            auth_style: AuthStyle::Bearer,
            api_key: "sk-secret".to_owned(),
            requested_model: "gpt-4o".to_owned(),
            upstream_model: "gpt-4o".to_owned(),
            headers: HashMap::new(),
        };
        let cloned = original.clone();
        assert_eq!(
            cloned, original,
            "cloned ProviderAdapterTargetConfig should equal original"
        );
    }
}
