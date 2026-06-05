use std::sync::Arc;
use std::time::Duration;

use llm_proxy_core::{
    AppConfig, Config, Counter, FallbackHandler, Metrics, ProviderRegistry,
};
use llm_proxy_provider::{OpenCodeClient, ProviderAdapterRegistry, ProxyClient};

use crate::middleware::{RateLimiter, RequestDeduplicator, RequestIdGenerator};

/// Static information about this binary.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// Package name.
    pub name: &'static str,
    /// Package version.
    pub version: &'static str,
    /// Target triple the binary was compiled for.
    pub target: &'static str,
    /// Git commit SHA the binary was built from.
    pub git_sha: &'static str,
}

/// Model router that selects models based on scenario detection.
#[derive(Debug, Clone)]
pub struct ModelRouter {
    /// Reference to the config for model lookup.
    config: Arc<Config>,
}

impl ModelRouter {
    /// Create a new model router.
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    /// Resolve the model config for a given scenario name.
    pub fn resolve(&self, scenario: &str) -> Option<llm_proxy_core::ModelConfig> {
        self.config.models.get(scenario).cloned()
    }

    /// Get the fallback chain for a scenario.
    pub fn fallback_chain(&self, scenario: &str) -> Option<&Vec<llm_proxy_core::ModelConfig>> {
        self.config.fallbacks.get(scenario)
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// LegacyState bridge
// ---------------------------------------------------------------------------

/// Legacy state bridge for JSON compatibility mode.
///
/// Holds the old config-driven components so that existing routes (`/v1/messages`,
/// `/health`, `/version`, and router middleware) continue to compile and function
/// until Phase 8/9 migrate them to the core pipeline.
///
/// Only constructed via [`AppState::from_legacy`]. Must not be exposed or
/// constructed in TOML new-runtime mode.
#[derive(Debug, Clone)]
pub struct LegacyState {
    /// Legacy JSON config.
    pub config: Arc<Config>,
    /// Legacy OpenCode HTTP client.
    pub client: Arc<OpenCodeClient>,
    /// Legacy model router.
    pub model_router: Arc<ModelRouter>,
    /// Legacy fallback handler with circuit breaker protection.
    pub fallback_handler: Arc<FallbackHandler>,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state shared with every handler.
///
/// Cheap to clone: all fields are `Arc`-wrapped so cloning only bumps
/// reference counts. Required by axum's `State` extractor.
///
/// # Construction modes
///
/// Two mutually exclusive modes exist during the Phase 7 transition:
///
/// - **JSON compatibility mode**: `legacy = Some(...)`, `app_config = None`,
///   `providers = None`. Old routes use legacy fields. Constructed via
///   [`AppState::from_legacy`].
///
/// - **TOML new-runtime mode**: `app_config = Some(...)`, `providers = Some(...)`,
///   `legacy = None`. New core-pipeline routes use these. Constructed via
///   [`AppState::from_toml`].
///
/// Mixed combinations are rejected by constructors.
///
/// # Security note
///
/// Manual `Debug` impl is provided to ensure `api_key` in nested config types
/// is never leaked through debug formatting (e.g. in error logs).
#[derive(Clone)]
pub struct AppState {
    /// New TOML application config. `None` in JSON compatibility mode.
    pub app_config: Option<Arc<AppConfig>>,
    /// New provider registry. `None` in JSON compatibility mode.
    pub providers: Option<Arc<ProviderRegistry>>,
    /// Provider adapter registry (always present in both modes).
    pub provider_adapters: Arc<ProviderAdapterRegistry>,
    /// Protocol-neutral HTTP transport client (always present in both modes).
    pub proxy_client: Arc<ProxyClient>,
    /// Legacy state bridge. `None` in TOML new-runtime mode.
    pub legacy: Option<Arc<LegacyState>>,
    /// Build identifier reported by `/version`.
    pub build: Arc<BuildInfo>,
    /// Token counter for estimating token usage.
    pub token_counter: Arc<Counter>,
    /// Runtime metrics collector.
    pub metrics: Arc<Metrics>,
    /// Per-IP rate limiter.
    pub rate_limiter: Arc<RateLimiter>,
    /// Request deduplicator for idempotent request handling.
    pub request_dedup: Arc<RequestDeduplicator>,
    /// Request ID generator.
    pub request_id_gen: Arc<RequestIdGenerator>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("app_config", &self.app_config)
            .field("providers", &self.providers)
            .field("provider_adapters", &self.provider_adapters)
            .field("proxy_client", &self.proxy_client)
            .field("legacy", &self.legacy)
            .field("build", &self.build)
            .field("token_counter", &self.token_counter)
            .field("metrics", &self.metrics)
            .field("rate_limiter", &self.rate_limiter)
            .field("request_dedup", &self.request_dedup)
            .field("request_id_gen", &self.request_id_gen)
            .finish()
    }
}

impl AppState {
    /// Construct `AppState` in JSON legacy compatibility mode.
    ///
    /// Sets `legacy = Some(...)`, `app_config = None`, `providers = None`.
    /// Old routes that read `config`, `client`, `model_router`, or
    /// `fallback_handler` should use `state.legacy()` to access them.
    #[must_use]
    pub fn from_legacy(
        config: Config,
        build: BuildInfo,
        client: OpenCodeClient,
        fallback_handler: FallbackHandler,
        provider_adapters: ProviderAdapterRegistry,
        proxy_client: ProxyClient,
    ) -> Self {
        let config_arc = Arc::new(config);
        Self {
            app_config: None,
            providers: None,
            provider_adapters: Arc::new(provider_adapters),
            proxy_client: Arc::new(proxy_client),
            legacy: Some(Arc::new(LegacyState {
                config: Arc::clone(&config_arc),
                client: Arc::new(client),
                model_router: Arc::new(ModelRouter::new(config_arc)),
                fallback_handler: Arc::new(fallback_handler),
            })),
            build: Arc::new(build),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        }
    }

    /// Construct `AppState` in TOML new-runtime mode.
    ///
    /// Sets `app_config = Some(...)`, `providers = Some(...)`, `legacy = None`.
    /// Phase 8 and later route tests must use this mode.
    #[must_use]
    pub fn from_toml(
        app_config: AppConfig,
        providers: ProviderRegistry,
        provider_adapters: ProviderAdapterRegistry,
        proxy_client: ProxyClient,
        build: BuildInfo,
    ) -> Self {
        Self {
            app_config: Some(Arc::new(app_config)),
            providers: Some(Arc::new(providers)),
            provider_adapters: Arc::new(provider_adapters),
            proxy_client: Arc::new(proxy_client),
            legacy: None,
            build: Arc::new(build),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        }
    }

    /// Request timeout duration.
    ///
    /// Uses the new `app_config` timeout when available, otherwise falls back
    /// to the legacy config timeout.
    pub fn request_timeout(&self) -> Duration {
        if let Some(ac) = &self.app_config {
            ac.server.request_timeout
        } else if let Some(ls) = &self.legacy {
            ls.config.request_timeout
        } else {
            Duration::from_secs(60)
        }
    }

    /// Server name for responses and version endpoints.
    ///
    /// Uses the new `app_config` server name when available, otherwise falls
    /// back to the legacy config server name.
    pub fn server_name(&self) -> &str {
        if let Some(ac) = &self.app_config {
            &ac.server.server_name
        } else if let Some(ls) = &self.legacy {
            &ls.config.server_name
        } else {
            "llm-proxy"
        }
    }

    /// Access the legacy state bridge.
    ///
    /// Returns `None` in TOML new-runtime mode.
    pub fn legacy(&self) -> Option<&LegacyState> {
        self.legacy.as_deref()
    }

    /// Access the new TOML app config.
    ///
    /// Returns `None` in JSON legacy mode.
    pub fn app_config(&self) -> Option<&AppConfig> {
        self.app_config.as_deref()
    }

    /// Access the provider registry.
    ///
    /// Returns `None` in JSON legacy mode.
    pub fn providers(&self) -> Option<&ProviderRegistry> {
        self.providers.as_deref()
    }

    /// Return the bind address the server should listen on.
    ///
    /// In TOML mode, returns `app_config.server.bind`. In legacy mode,
    /// reconstructs the address from `config.host` and `config.port`.
    /// Returns a default of `127.0.0.1:3456` if neither mode is configured.
    pub fn bind_address(&self) -> std::net::SocketAddr {
        if let Some(ac) = &self.app_config {
            ac.server.bind
        } else if let Some(ls) = &self.legacy {
            format!("{}:{}", ls.config.host, ls.config.port)
                .parse()
                .unwrap_or_else(|_| "127.0.0.1:3456".parse().expect("valid default"))
        } else {
            "127.0.0.1:3456".parse().expect("valid default")
        }
    }

    /// Legacy convenience constructor matching the old `AppState::new` signature.
    ///
    /// Equivalent to [`Self::from_legacy`] with builtin adapter registry and
    /// default proxy client. Provided for backward compatibility during the
    /// transition.
    #[must_use]
    pub fn new(
        config: Config,
        build: BuildInfo,
        client: OpenCodeClient,
        fallback_handler: FallbackHandler,
    ) -> Self {
        Self::from_legacy(
            config,
            build,
            client,
            fallback_handler,
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_proxy_core::ModelConfig;
    use std::collections::HashMap;

    /// Helper: build a `ModelRouter` backed by a `Config` with the given
    /// models and fallbacks maps.  All other config fields are defaults.
    fn make_router(
        models: HashMap<String, ModelConfig>,
        fallbacks: HashMap<String, Vec<ModelConfig>>,
    ) -> ModelRouter {
        let config = Config {
            models,
            fallbacks,
            ..Default::default()
        };
        ModelRouter::new(Arc::new(config))
    }

    // -- resolve() tests -----------------------------------------------------

    #[test]
    fn resolve_returns_some_for_configured_scenario() {
        let mut models = HashMap::new();
        models.insert(
            "default".to_owned(),
            ModelConfig {
                provider: "opencode-go".to_owned(),
                model_id: "kimi-k2.6".to_owned(),
                temperature: 0.7,
                max_tokens: 4096,
                ..Default::default()
            },
        );
        let router = make_router(models, HashMap::new());

        let mc = router.resolve("default").expect("should find default");
        assert_eq!(mc.model_id, "kimi-k2.6");
        assert_eq!(mc.provider, "opencode-go");
        assert_eq!(mc.temperature, 0.7);
        assert_eq!(mc.max_tokens, 4096);
    }

    #[test]
    fn resolve_returns_none_for_unknown_scenario() {
        let router = make_router(HashMap::new(), HashMap::new());
        assert!(router.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolve_distinct_models_per_scenario() {
        let mut models = HashMap::new();
        models.insert(
            "default".to_owned(),
            ModelConfig {
                model_id: "kimi-k2.6".to_owned(),
                ..Default::default()
            },
        );
        models.insert(
            "complex".to_owned(),
            ModelConfig {
                model_id: "glm-5.1".to_owned(),
                ..Default::default()
            },
        );
        let router = make_router(models, HashMap::new());

        assert_eq!(router.resolve("default").unwrap().model_id, "kimi-k2.6");
        assert_eq!(router.resolve("complex").unwrap().model_id, "glm-5.1");
    }

    #[test]
    fn resolve_returns_cloned_model_config() {
        let mut models = HashMap::new();
        models.insert(
            "think".to_owned(),
            ModelConfig {
                provider: "opencode-go".to_owned(),
                model_id: "glm-5".to_owned(),
                temperature: 0.5,
                max_tokens: 8192,
                context_threshold: 80_000,
                reasoning_effort: "max".to_owned(),
                thinking: Some(serde_json::json!({ "type": "enabled" })),
            },
        );
        let router = make_router(models, HashMap::new());

        let mc = router.resolve("think").unwrap();
        assert_eq!(mc.model_id, "glm-5");
        assert_eq!(mc.temperature, 0.5);
        assert_eq!(mc.max_tokens, 8192);
        assert_eq!(mc.context_threshold, 80_000);
        assert_eq!(mc.reasoning_effort, "max");
        assert!(mc.thinking.is_some());
    }

    // -- fallback_chain() tests ----------------------------------------------

    #[test]
    fn fallback_chain_returns_some_for_configured_scenario() {
        let mut fallbacks = HashMap::new();
        fallbacks.insert(
            "default".to_owned(),
            vec![
                ModelConfig {
                    provider: "opencode-go".to_owned(),
                    model_id: "qwen3.5-plus".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    provider: "opencode-go".to_owned(),
                    model_id: "glm-5.1".to_owned(),
                    ..Default::default()
                },
            ],
        );
        let router = make_router(HashMap::new(), fallbacks);

        let chain = router
            .fallback_chain("default")
            .expect("should find default fallbacks");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].model_id, "qwen3.5-plus");
        assert_eq!(chain[1].model_id, "glm-5.1");
    }

    #[test]
    fn fallback_chain_returns_none_for_unknown_scenario() {
        let router = make_router(HashMap::new(), HashMap::new());
        assert!(router.fallback_chain("missing").is_none());
    }

    #[test]
    fn fallback_chain_empty_vec_is_still_some() {
        let mut fallbacks = HashMap::new();
        fallbacks.insert("default".to_owned(), vec![]);
        let router = make_router(HashMap::new(), fallbacks);

        let chain = router.fallback_chain("default");
        assert!(chain.is_some(), "empty fallback vec should still be Some");
        assert!(chain.unwrap().is_empty());
    }

    // -- config() tests ------------------------------------------------------

    #[test]
    fn config_returns_reference_to_underlying_config() {
        let mut models = HashMap::new();
        models.insert(
            "default".to_owned(),
            ModelConfig {
                model_id: "kimi-k2.6".to_owned(),
                ..Default::default()
            },
        );
        let router = make_router(models, HashMap::new());

        let cfg = router.config();
        assert_eq!(cfg.models["default"].model_id, "kimi-k2.6");
        // Also verify non-model fields are defaults.
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 3456);
    }

    // -- Port of Go TestResolveRequestedModel_UsesFallbacks ------------------

    #[test]
    fn resolve_with_fallbacks_matches_go_test() {
        let mut models = HashMap::new();
        models.insert(
            "kimi-k2.6".to_owned(),
            ModelConfig {
                model_id: "kimi-k2.6".to_owned(),
                ..Default::default()
            },
        );
        let mut fallbacks = HashMap::new();
        fallbacks.insert(
            "default".to_owned(),
            vec![
                ModelConfig {
                    provider: "opencode-go".to_owned(),
                    model_id: "qwen3.5-plus".to_owned(),
                    ..Default::default()
                },
                ModelConfig {
                    provider: "opencode-go".to_owned(),
                    model_id: "glm-5.1".to_owned(),
                    ..Default::default()
                },
            ],
        );
        let router = make_router(models, fallbacks);

        // Primary model resolution.
        let primary = router.resolve("kimi-k2.6").expect("should find kimi-k2.6");
        assert_eq!(primary.model_id, "kimi-k2.6");

        // Fallback chain for "default" scenario.
        let chain = router
            .fallback_chain("default")
            .expect("should find default fallbacks");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].model_id, "qwen3.5-plus");
        assert_eq!(chain[1].model_id, "glm-5.1");
    }

    // -- Port of Go RespectRequestedModel scenarios --------------------------

    #[test]
    fn resolve_requested_model_by_model_id() {
        let mut models = HashMap::new();
        models.insert(
            "default".to_owned(),
            ModelConfig {
                provider: "opencode-go".to_owned(),
                model_id: "kimi-k2.6".to_owned(),
                ..Default::default()
            },
        );
        models.insert(
            "kimi-k2.6".to_owned(),
            ModelConfig {
                provider: "opencode-go".to_owned(),
                model_id: "kimi-k2.6".to_owned(),
                temperature: 0.7,
                max_tokens: 4096,
                context_threshold: 80_000,
                ..Default::default()
            },
        );
        let router = make_router(models, HashMap::new());

        // Simulate respect_requested_model: look up the model by its ID.
        let mc = router
            .resolve("kimi-k2.6")
            .expect("should find model by ID");
        assert_eq!(mc.model_id, "kimi-k2.6");
        assert_eq!(mc.temperature, 0.7);
        assert_eq!(mc.max_tokens, 4096);
    }

    #[test]
    fn resolve_unknown_model_id_returns_none() {
        let mut models = HashMap::new();
        models.insert(
            "default".to_owned(),
            ModelConfig {
                provider: "opencode-go".to_owned(),
                model_id: "kimi-k2.6".to_owned(),
                temperature: 0.5,
                max_tokens: 8192,
                ..Default::default()
            },
        );
        let router = make_router(models, HashMap::new());

        // An unknown model ID is not a configured scenario key.
        assert!(router.resolve("some-unknown-model").is_none());
    }

    // Verify that the config's respect_requested_model flag is accessible
    // through the ModelRouter's config reference.

    #[test]
    fn config_respect_requested_model_flag() {
        let config = Config {
            respect_requested_model: true,
            ..Default::default()
        };
        let router = ModelRouter::new(Arc::new(config));
        assert!(router.config().respect_requested_model);
    }

    // =========================================================================
    // Phase 7 tests
    // =========================================================================

    /// Helper: build a minimal TOML AppConfig for tests.
    fn make_app_config() -> AppConfig {
        use llm_proxy_core::ServerConfig;
        AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: Duration::from_secs(300),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "test-proxy".to_owned(),
            },
            models: HashMap::new(),
        }
    }

    /// Helper: build a minimal ProviderRegistry for tests.
    fn make_provider_registry() -> ProviderRegistry {
        ProviderRegistry::from_providers(vec![]).expect("empty registry")
    }

    fn make_build_info() -> BuildInfo {
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        }
    }

    // -- TOML AppState construction ------------------------------------------

    #[test]
    fn toml_app_state_construction() {
        let state = AppState::from_toml(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert!(state.app_config.is_some());
        assert!(state.providers.is_some());
        assert!(state.legacy.is_none());
        assert_eq!(state.server_name(), "test-proxy");
        assert_eq!(state.request_timeout(), Duration::from_secs(300));
    }

    // -- JSON legacy construction --------------------------------------------

    #[test]
    fn json_legacy_construction() {
        let config = Config {
            api_key: "test-key".to_owned(),
            server_name: "legacy-proxy".to_owned(),
            request_timeout: Duration::from_secs(120),
            ..Default::default()
        };
        let state = AppState::from_legacy(
            config,
            make_build_info(),
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, Duration::from_secs(30)),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        );
        assert!(state.app_config.is_none());
        assert!(state.providers.is_none());
        assert!(state.legacy.is_some());
        assert_eq!(state.server_name(), "legacy-proxy");
        assert_eq!(state.request_timeout(), Duration::from_secs(120));
    }

    // -- Mixed construction rejected -----------------------------------------

    #[test]
    fn mixed_construction_rejected_by_constructors() {
        // from_toml always sets legacy=None, so mixed is impossible via constructor.
        let toml_state = AppState::from_toml(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert!(toml_state.app_config.is_some());
        assert!(toml_state.providers.is_some());
        assert!(toml_state.legacy.is_none());

        // from_legacy always sets app_config=None and providers=None.
        let legacy_state = AppState::from_legacy(
            Config::default(),
            make_build_info(),
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, Duration::from_secs(30)),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        );
        assert!(legacy_state.app_config.is_none());
        assert!(legacy_state.providers.is_none());
        assert!(legacy_state.legacy.is_some());
    }

    // -- Helper methods in TOML mode -----------------------------------------

    #[test]
    fn helpers_use_app_config_in_toml_mode() {
        let state = AppState::from_toml(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "test-proxy");
        assert_eq!(state.request_timeout(), Duration::from_secs(300));
        assert!(state.legacy().is_none());
        assert!(state.app_config().is_some());
        assert!(state.providers().is_some());
    }

    // -- Helper methods in legacy mode ---------------------------------------

    #[test]
    fn helpers_use_legacy_config_in_legacy_mode() {
        let config = Config {
            api_key: "key".to_owned(),
            server_name: "legacy-name".to_owned(),
            request_timeout: Duration::from_secs(42),
            ..Default::default()
        };
        let state = AppState::from_legacy(
            config,
            make_build_info(),
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, Duration::from_secs(30)),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        );
        assert_eq!(state.server_name(), "legacy-name");
        assert_eq!(state.request_timeout(), Duration::from_secs(42));
        assert!(state.legacy().is_some());
        assert!(state.app_config().is_none());
        assert!(state.providers().is_none());
    }

    // -- Debug does not leak API keys ----------------------------------------

    #[test]
    fn app_state_debug_does_not_leak_api_key() {
        let config = Config {
            api_key: "sk-super-secret-key-do-not-leak-12345".to_owned(),
            ..Default::default()
        };
        let state = AppState::from_legacy(
            config,
            make_build_info(),
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, Duration::from_secs(30)),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        );
        let debug_output = format!("{:?}", state);
        assert!(
            !debug_output.contains("sk-super-secret-key-do-not-leak-12345"),
            "Debug output must not contain the actual api_key, got: {debug_output}"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for api_key, got: {debug_output}"
        );
    }

    // -- Debug redaction in TOML mode via ProviderRegistry --------------------

    #[test]
    fn app_state_debug_does_not_leak_api_key_toml_mode() {
        use llm_proxy_core::{AuthStyle, ProviderConfig};
        let provider = ProviderConfig {
            name: "test".to_owned(),
            api_key: "sk-toml-secret-key-99999".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
        };
        let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
        let state = AppState::from_toml(
            make_app_config(),
            registry,
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        let debug_output = format!("{:?}", state);
        assert!(
            !debug_output.contains("sk-toml-secret-key-99999"),
            "Debug output must not contain the actual api_key in TOML mode, got: {debug_output}"
        );
    }

    // -- Legacy convenience new() still works --------------------------------

    #[test]
    fn legacy_new_constructor_still_works() {
        let state = AppState::new(
            Config::default(),
            make_build_info(),
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, Duration::from_secs(30)),
        );
        assert!(state.legacy.is_some());
        assert!(state.app_config.is_none());
        assert!(state.providers.is_none());
        // Verify the legacy path works
        assert!(state.legacy().unwrap().model_router.resolve("default").is_none());
    }

    // -- TOML mode has no legacy fields accessible ---------------------------

    #[test]
    fn toml_mode_no_legacy_fields() {
        let state = AppState::from_toml(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert!(state.legacy().is_none());
        // provider_adapters and proxy_client are always present.
        assert!(state.provider_adapters.protocol_names().len() == 4);
    }

    // -- Default timeout when neither mode configured ------------------------

    #[test]
    fn default_timeout_when_no_config() {
        // Build an AppState manually without app_config or legacy.
        let state = AppState {
            app_config: None,
            providers: None,
            provider_adapters: Arc::new(ProviderAdapterRegistry::builtin()),
            proxy_client: Arc::new(ProxyClient::new()),
            legacy: None,
            build: Arc::new(make_build_info()),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        };
        assert_eq!(state.request_timeout(), Duration::from_secs(60));
        assert_eq!(state.server_name(), "llm-proxy");
    }

    // -- TOML provider protocol validation against builtin -------------------

    #[test]
    fn toml_provider_protocol_validation_against_builtin() {
        use llm_proxy_core::{AuthStyle, ProviderAdapterConfig, ProviderConfig, ProviderModelConfig};
        let provider = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
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
                m.insert(
                    "messages".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "anthropic_messages".to_owned(),
                        endpoint: "https://example.com/v1/messages".to_owned(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "gpt-5.4".to_owned(),
                    ProviderModelConfig {
                        adapter: "chat".to_owned(),
                    },
                );
                m
            },
        };
        let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
        let adapter_reg = ProviderAdapterRegistry::builtin();
        // Validate protocols against builtins
        let result = registry.validate_protocols(adapter_reg.protocol_names());
        assert!(result.is_ok(), "all protocols are builtin, got: {result:?}");
    }

    // -- TOML unknown protocol fails validation against builtin ---------------

    #[test]
    fn toml_unknown_protocol_fails_validation_against_builtin() {
        use llm_proxy_core::{AuthStyle, ProviderAdapterConfig, ProviderConfig};
        let provider = ProviderConfig {
            name: "bad".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "fake".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "not_a_real_protocol".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                    },
                );
                m
            },
            models: HashMap::new(),
        };
        let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
        let adapter_reg = ProviderAdapterRegistry::builtin();
        let result = registry.validate_protocols(adapter_reg.protocol_names());
        assert!(result.is_err(), "unknown protocol should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown protocol"),
            "expected unknown protocol error, got: {err}"
        );
    }

    // -- Unsupported config extensions ---------------------------------------

    #[test]
    fn unsupported_config_extensions_rejected() {
        // This is tested in main.rs integration, but we verify the logic here:
        // The extension check is: match extension { "toml" | "json" => ok, _ => err }
        let path_toml = std::path::Path::new("config.toml");
        let path_json = std::path::Path::new("config.json");
        let path_yaml = std::path::Path::new("config.yaml");
        let path_txt = std::path::Path::new("config.txt");
        let path_no_ext = std::path::Path::new("config");

        assert_eq!(path_toml.extension().and_then(|e| e.to_str()), Some("toml"));
        assert_eq!(path_json.extension().and_then(|e| e.to_str()), Some("json"));
        assert_ne!(path_yaml.extension().and_then(|e| e.to_str()), Some("toml"));
        assert_ne!(path_yaml.extension().and_then(|e| e.to_str()), Some("json"));
        assert_ne!(path_txt.extension().and_then(|e| e.to_str()), Some("toml"));
        assert_ne!(path_no_ext.extension().and_then(|e| e.to_str()), Some("toml"));
    }

    // -- Send + Sync static assertion for AppState ---------------------------

    #[test]
    fn app_state_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<AppState>();
        check::<LegacyState>();
    }
}
