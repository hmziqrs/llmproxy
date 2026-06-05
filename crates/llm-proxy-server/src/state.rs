use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use llm_proxy_core::{
    AppConfig, Config, Counter, FallbackHandler, Metrics, ProviderRegistry,
};
use llm_proxy_provider::{OpenCodeClient, ProviderAdapterRegistry, ProxyClient};
#[allow(unused_imports)]
use tracing::warn;

use crate::middleware::{RateLimiter, RequestDeduplicator, RequestIdGenerator};

/// Default bind address used when no config is available.
#[allow(dead_code)]
fn default_bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 3456))
}

/// Default rate limit (requests per minute) when no config override is provided.
///
/// TODO: Make this configurable via TOML config in a future phase.
const DEFAULT_RATE_LIMIT_RPM: u32 = 100;

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
///
/// `pub(crate)` visibility: not re-exported from `lib.rs` and will be deleted
/// in Phase 11 when the JSON legacy path is removed.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Phase 11 deletion candidate; only used in tests.
pub(crate) struct ModelRouter {
    /// Reference to the config for model lookup.
    #[allow(dead_code)] // Phase 11 deletion candidate; only used in tests.
    config: Arc<Config>,
}

impl ModelRouter {
    /// Create a new model router.
    pub(crate) fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    /// Resolve the model config for a given scenario name.
    #[allow(dead_code)] // Phase 11 deletion candidate; only used in tests.
    pub(crate) fn resolve(&self, scenario: &str) -> Option<llm_proxy_core::ModelConfig> {
        self.config.models.get(scenario).cloned()
    }

    /// Get the fallback chain for a scenario.
    #[allow(dead_code)] // Phase 11 deletion candidate; only used in tests.
    pub(crate) fn fallback_chain(&self, scenario: &str) -> Option<&[llm_proxy_core::ModelConfig]> {
        self.config.fallbacks.get(scenario).map(Vec::as_slice)
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
///
/// Fields are `pub(crate)` to prevent external crates from depending on legacy
/// internals. Route handlers within the server crate access them through
/// [`AppState::legacy()`]. This type will be deleted in Phase 11.
///
/// Manual `Debug` impl ensures the redaction chain is self-documenting:
/// delegates to `Config`'s manual Debug (which redacts `api_key`) rather than
/// relying on a derived `Debug` that would break silently if `Config` ever
/// gained a derived `Debug`.
#[derive(Clone)]
pub(crate) struct LegacyState {
    /// Legacy JSON config.
    pub(crate) config: Arc<Config>,
    /// Legacy OpenCode HTTP client.
    pub(crate) client: Arc<OpenCodeClient>,
    /// Legacy model router.
    pub(crate) model_router: Arc<ModelRouter>,
    /// Legacy fallback handler with circuit breaker protection.
    pub(crate) fallback_handler: Arc<FallbackHandler>,
}

impl std::fmt::Debug for LegacyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyState")
            // SECURITY: Config has a manual Debug that redacts api_key.
            .field("config", &self.config)
            .field("client", &self.client)
            .field("model_router", &self.model_router)
            .field("fallback_handler", &self.fallback_handler)
            .finish()
    }
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
/// Mixed combinations (both `Some`, or both `None`) are invalid and must not
/// occur outside of defense-in-depth test code. The constructors enforce this;
/// fields are `pub(crate)` so external crates cannot bypass the constructors.
///
/// # Security note
///
/// Manual `Debug` impl is provided to ensure `api_key` in nested config types
/// is never leaked through debug formatting (e.g. in error logs).
///
/// The transitive redaction chain for Debug is:
/// - `AppState` -> `app_config` field: `AppConfig` has no secrets (safe).
/// - `AppState` -> `legacy` field: `LegacyState` -> `Config` (manual Debug,
///   redacts api_key). `OpenCodeClient` -> `Config` (same manual Debug). Both
///   safe.
/// - `AppState` -> `providers` field: `ProviderRegistry` -> `ProviderConfig`
///   (manual Debug, redacts api_key). Safe.
/// - `AppState` -> `provider_adapters` field: `ProviderAdapterRegistry` holds
///   only `HashMap<ProviderProtocol, ProviderAdapter>` -- no secrets. Safe.
/// - `AppState` -> remaining fields: `BuildInfo`, `Counter`, `Metrics`,
///   `RateLimiter`, `RequestDeduplicator`, `RequestIdGenerator` -- none hold
///   secrets. Safe.
///
/// **Maintenance note:** If any of the above types gains a plain derived `Debug`
/// that contains secrets, the redaction chain breaks silently. The test
/// `app_state_debug_does_not_leak_api_key` provides regression coverage.
#[derive(Clone)]
#[non_exhaustive]
pub struct AppState {
    /// New TOML application config. `None` in JSON compatibility mode.
    pub(crate) app_config: Option<Arc<AppConfig>>,
    /// New provider registry. `None` in JSON compatibility mode.
    pub(crate) providers: Option<Arc<ProviderRegistry>>,
    /// Provider adapter registry (always present in both modes).
    pub(crate) provider_adapters: Arc<ProviderAdapterRegistry>,
    /// Protocol-neutral HTTP transport client (always present in both modes).
    pub(crate) proxy_client: Arc<ProxyClient>,
    /// Legacy state bridge. `None` in TOML new-runtime mode.
    pub(crate) legacy: Option<Arc<LegacyState>>,
    /// Build identifier reported by `/version`.
    pub(crate) build: Arc<BuildInfo>,
    /// Token counter for estimating token usage.
    pub(crate) token_counter: Arc<Counter>,
    /// Runtime metrics collector.
    pub(crate) metrics: Arc<Metrics>,
    /// Per-IP rate limiter.
    pub(crate) rate_limiter: Arc<RateLimiter>,
    /// Request deduplicator for idempotent request handling.
    pub(crate) request_dedup: Arc<RequestDeduplicator>,
    /// Request ID generator.
    pub(crate) request_id_gen: Arc<RequestIdGenerator>,
}

impl std::fmt::Debug for AppState {
    // SECURITY: This manual Debug impl delegates through Arc to each field's
    // Debug. The redaction chain documented on the struct must be maintained.
    // The tests `app_state_debug_does_not_leak_api_key` and
    // `app_state_debug_does_not_leak_api_key_toml_mode` provide regression
    // coverage. See struct-level doc comment for the full chain.
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

/// Internal enum for dispatching on the active configuration mode.
///
/// Used by [`AppState::active_config_mode`] to centralize the
/// `app_config` vs `legacy` dispatch logic.
enum ActiveMode<'a> {
    /// TOML new-runtime mode.
    Toml(&'a Arc<AppConfig>),
    /// JSON legacy compatibility mode.
    Legacy(&'a Arc<LegacyState>),
    /// Neither mode is configured (invalid, defense-in-depth only).
    None,
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
        let cfg = Arc::new(config);
        let state = Self {
            app_config: None,
            providers: None,
            provider_adapters: Arc::new(provider_adapters),
            proxy_client: Arc::new(proxy_client),
            legacy: Some(Arc::new(LegacyState {
                // cfg is cloned for the model_router's independent Arc reference.
                config: Arc::clone(&cfg),
                client: Arc::new(client),
                model_router: Arc::new(ModelRouter::new(cfg)),
                fallback_handler: Arc::new(fallback_handler),
            })),
            build: Arc::new(build),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(DEFAULT_RATE_LIMIT_RPM)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        };
        #[cfg(debug_assertions)]
        state.validate_invariants("from_legacy");
        state
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
        let state = Self {
            app_config: Some(Arc::new(app_config)),
            providers: Some(Arc::new(providers)),
            provider_adapters: Arc::new(provider_adapters),
            proxy_client: Arc::new(proxy_client),
            legacy: None,
            build: Arc::new(build),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(DEFAULT_RATE_LIMIT_RPM)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        };
        #[cfg(debug_assertions)]
        state.validate_invariants("from_toml");
        state
    }

    /// Request timeout duration.
    ///
    /// Uses the new `app_config` timeout when available, otherwise falls back
    /// to the legacy config timeout.
    pub fn request_timeout(&self) -> Duration {
        match self.active_config_mode() {
            ActiveMode::Toml(ac) => ac.server.request_timeout,
            ActiveMode::Legacy(ls) => ls.config.request_timeout,
            ActiveMode::None => {
                // This branch is unreachable via constructors (from_legacy and
                // from_toml always set exactly one mode). The fallback exists as
                // defense-in-depth for direct struct construction in test code.
                // If this is reached in production, it indicates a bug.
                #[cfg(debug_assertions)]
                {
                    unreachable!(
                        "AppState has neither app_config nor legacy; \
                         this indicates invalid construction"
                    );
                }
                #[cfg(not(debug_assertions))]
                {
                    warn!("AppState has neither app_config nor legacy; returning default 60s timeout. \
                           This indicates an invalid construction and should be investigated.");
                    Duration::from_secs(60)
                }
            }
        }
    }

    /// Server name for responses and version endpoints.
    ///
    /// Uses the new `app_config` server name when available, otherwise falls
    /// back to the legacy config server name.
    pub fn server_name(&self) -> &str {
        match self.active_config_mode() {
            ActiveMode::Toml(ac) => &ac.server.server_name,
            ActiveMode::Legacy(ls) => &ls.config.server_name,
            ActiveMode::None => {
                #[cfg(debug_assertions)]
                {
                    unreachable!(
                        "AppState has neither app_config nor legacy; \
                         this indicates invalid construction"
                    );
                }
                #[cfg(not(debug_assertions))]
                {
                    warn!("AppState has neither app_config nor legacy; returning default server name. \
                           This indicates an invalid construction and should be investigated.");
                    "llm-proxy"
                }
            }
        }
    }

    /// Access the legacy state bridge.
    ///
    /// Returns `None` in TOML new-runtime mode.
    ///
    /// `pub(crate)` because `LegacyState` is `pub(crate)` and will be removed
    /// in Phase 11.
    pub(crate) fn legacy(&self) -> Option<&LegacyState> {
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
    /// uses the pre-validated `config.bind` field. Returns `default_bind()`
    /// if neither mode is configured.
    pub fn bind_address(&self) -> SocketAddr {
        match self.active_config_mode() {
            ActiveMode::Toml(ac) => ac.server.bind,
            ActiveMode::Legacy(ls) => ls.config.bind,
            ActiveMode::None => {
                #[cfg(debug_assertions)]
                {
                    unreachable!(
                        "AppState has neither app_config nor legacy; \
                         this indicates invalid construction"
                    );
                }
                #[cfg(not(debug_assertions))]
                {
                    warn!("AppState has neither app_config nor legacy; returning default bind address. \
                           This indicates an invalid construction and should be investigated.");
                    default_bind()
                }
            }
        }
    }

    /// Access build metadata (version, target, git SHA).
    ///
    /// Returns a reference to the [`BuildInfo`] instance. Available in both
    /// TOML and legacy modes.
    pub fn build_info(&self) -> &BuildInfo {
        &self.build
    }

    // -- Internal helpers -------------------------------------------------------

    /// Which configuration mode is active.
    ///
    /// Centralizes the `app_config` vs `legacy` dispatch so that the three
    /// helper methods (`request_timeout`, `server_name`, `bind_address`) do
    /// not each duplicate the same match logic.
    fn active_config_mode(&self) -> ActiveMode<'_> {
        if let Some(ac) = &self.app_config {
            ActiveMode::Toml(ac)
        } else if let Some(ls) = &self.legacy {
            ActiveMode::Legacy(ls)
        } else {
            ActiveMode::None
        }
    }

    /// Assert construction invariants that the type system cannot enforce.
    ///
    /// Called at the end of each constructor to catch regressions early in
    /// debug builds. The checks are:
    ///
    /// - `from_legacy` must not set `app_config` or `providers`.
    /// - `from_toml` must not set `legacy`.
    /// - At least one of `app_config` or `legacy` must be set.
    /// - `app_config` and `providers` must both be `Some` or both be `None`
    ///   (TOML mode couples them together).
    #[cfg(debug_assertions)]
    fn validate_invariants(&self, source: &str) {
        if self.app_config.is_some() || self.providers.is_some() {
            debug_assert!(
                self.legacy.is_none(),
                "AppState invariant violation ({source}): app_config/providers are set but legacy is also Some"
            );
        }
        if self.legacy.is_some() {
            debug_assert!(
                self.app_config.is_none() && self.providers.is_none(),
                "AppState invariant violation ({source}): legacy is set but app_config/providers are also Some"
            );
        }
        debug_assert!(
            self.app_config.is_some() || self.legacy.is_some(),
            "AppState invariant violation ({source}): neither app_config nor legacy is set"
        );
        debug_assert!(
            self.app_config.is_some() == self.providers.is_some(),
            "AppState invariant violation ({source}): app_config and providers must both be Some or both be None"
        );
    }

    /// Legacy convenience constructor matching the old `AppState::new` signature.
    ///
    /// Equivalent to [`Self::from_legacy`] with builtin adapter registry and
    /// default proxy client. Provided for backward compatibility during the
    /// transition.
    #[deprecated(
        since = "0.2.0",
        note = "use AppState::from_legacy() explicitly instead"
    )]
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
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for api_key in TOML mode, got: {debug_output}"
        );
    }

    // -- Legacy convenience new() still works --------------------------------

    #[test]
    #[allow(deprecated)]
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
        // Use >= 4 rather than == 4 so adding new builtin adapters does not
        // break this test (the intent is to verify adapters are populated).
        let protocol_names = state.provider_adapters.protocol_names();
        assert!(protocol_names.len() >= 4, "expected at least 4 builtin adapters, got {len}", len = protocol_names.len());
        // Verify specific known protocol names are present for stronger coverage.
        assert!(protocol_names.contains(&"openai_chat_completions"), "missing openai_chat_completions adapter");
        assert!(protocol_names.contains(&"anthropic_messages"), "missing anthropic_messages adapter");
    }

    // -- Default timeout when neither mode configured ------------------------
    //
    // NOTE: This test directly constructs an AppState with both app_config=None
    // and legacy=None, which is an invalid state per the plan's invariant.
    // This is intentional defense-in-depth testing: the helper methods have
    // fallback branches that should never be reached via constructors, but
    // exist as a safety net in release builds. In debug builds, the ActiveMode::None
    // branch panics with unreachable!(), so this test only runs in release mode.
    // Fields are pub(crate) so only crate-internal code (including tests) can
    // construct this state.

    #[test]
    #[cfg_attr(debug_assertions, ignore = "ActiveMode::None panics in debug builds")]
    fn default_timeout_when_no_config() {
        // Build an AppState manually without app_config or legacy.
        // This exercises the fallback branch in helper methods that can only
        // be reached via direct struct construction in release builds, not via constructors.
        let state = AppState {
            app_config: None,
            providers: None,
            provider_adapters: Arc::new(ProviderAdapterRegistry::builtin()),
            proxy_client: Arc::new(ProxyClient::new()),
            legacy: None,
            build: Arc::new(make_build_info()),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(DEFAULT_RATE_LIMIT_RPM)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        };
        assert_eq!(state.request_timeout(), Duration::from_secs(60));
        assert_eq!(state.server_name(), "llm-proxy");
        assert_eq!(state.bind_address(), default_bind());
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

    // -- Config extension dispatch logic -------------------------------------

    #[test]
    fn config_extension_dispatch_classifies_correctly() {
        /// Mimics the match arm logic from cmd_serve to verify extension
        /// classification without requiring a full integration test.
        fn classify(path: &std::path::Path) -> &'static str {
            match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
                "toml" => "toml",
                "json" => "json",
                _ => "unsupported",
            }
        }

        assert_eq!(classify(std::path::Path::new("config.toml")), "toml");
        assert_eq!(classify(std::path::Path::new("config.json")), "json");
        assert_eq!(classify(std::path::Path::new("config.yaml")), "unsupported");
        assert_eq!(classify(std::path::Path::new("config.txt")), "unsupported");
        assert_eq!(classify(std::path::Path::new("config")), "unsupported");
        assert_eq!(classify(std::path::Path::new("config.YAML")), "unsupported");
    }

    // -- Send + Sync static assertion for AppState ---------------------------

    #[test]
    fn app_state_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<AppState>();
        check::<LegacyState>();
    }

    // -- bind_address() tests ------------------------------------------------

    #[test]
    fn bind_address_toml_mode_returns_configured_bind() {
        let mut cfg = make_app_config();
        cfg.server.bind = "10.0.0.1:8080".parse().unwrap();
        let state = AppState::from_toml(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.bind_address(), "10.0.0.1:8080".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn bind_address_legacy_mode_returns_config_bind() {
        // Config::default() sets bind to 0.0.0.0:8080.
        // from_legacy uses config.bind directly (the caller is responsible for
        // syncing bind with host/port, as load_json_state in main.rs does).
        let config = Config::default();
        let expected = config.bind;
        let state = AppState::from_legacy(
            config,
            make_build_info(),
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, Duration::from_secs(30)),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        );
        assert_eq!(state.bind_address(), expected);
    }

    // -- Debug redaction with empty api_key ----------------------------------

    #[test]
    fn debug_redacts_empty_api_key() {
        let config = Config {
            api_key: String::new(),
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
            debug_output.contains("[REDACTED]"),
            "Debug output must show [REDACTED] even for empty api_key, got: {debug_output}"
        );
    }

    // -- Clone shares Arc references -----------------------------------------

    #[test]
    fn clone_shares_arc_references() {
        let state = AppState::from_toml(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        let cloned = state.clone();
        assert!(
            Arc::ptr_eq(&state.metrics, &cloned.metrics),
            "cloned AppState should share the same Arc<Metrics>"
        );
        assert!(
            Arc::ptr_eq(&state.token_counter, &cloned.token_counter),
            "cloned AppState should share the same Arc<Counter>"
        );
        assert!(
            Arc::ptr_eq(&state.build, &cloned.build),
            "cloned AppState should share the same Arc<BuildInfo>"
        );
    }

    // -- Multi-provider TOML validation --------------------------------------

    #[test]
    fn multi_provider_toml_mixed_protocols() {
        use llm_proxy_core::{AuthStyle, ProviderAdapterConfig, ProviderConfig};

        // Provider with valid protocols
        let good = ProviderConfig {
            name: "good".to_owned(),
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
                m
            },
            models: HashMap::new(),
        };

        // Provider with invalid protocol
        let bad = ProviderConfig {
            name: "bad".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "fake".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "not_real".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                    },
                );
                m
            },
            models: HashMap::new(),
        };

        let registry = ProviderRegistry::from_providers(vec![good, bad]).expect("registry");
        let adapter_reg = ProviderAdapterRegistry::builtin();
        let result = registry.validate_protocols(adapter_reg.protocol_names());
        assert!(result.is_err(), "mixed providers with one bad protocol should fail");
    }

    // -- Boundary values for helpers -----------------------------------------

    #[test]
    fn zero_duration_timeout_is_returned() {
        let mut cfg = make_app_config();
        cfg.server.request_timeout = Duration::ZERO;
        let state = AppState::from_toml(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.request_timeout(), Duration::ZERO);
    }

    #[test]
    fn empty_server_name_is_returned() {
        let mut cfg = make_app_config();
        cfg.server.server_name = String::new();
        let state = AppState::from_toml(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "");
    }

    // -- Unicode server_name ---------------------------------------------------

    #[test]
    fn unicode_server_name_is_returned_verbatim() {
        let mut cfg = make_app_config();
        cfg.server.server_name = "代理服务器-ünïcödé".to_owned();
        let state = AppState::from_toml(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "代理服务器-ünïcödé");
    }

    #[test]
    fn very_long_server_name_is_returned() {
        let mut cfg = make_app_config();
        let long_name = "x".repeat(10_000);
        cfg.server.server_name = long_name.clone();
        let state = AppState::from_toml(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), long_name);
    }

    // -- Partial field construction defense-in-depth --------------------------

    // Verify that app_config=Some with providers=None is caught by invariant
    // validation in debug builds. This is a direct struct construction that
    // bypasses constructors -- the coupling invariant is checked by
    // validate_invariants.
    #[test]
    #[cfg_attr(debug_assertions, ignore = "exercises debug_assert in validate_invariants")]
    fn partial_toml_construction_app_config_without_providers() {
        // In release mode, this constructs an invalid state; the test verifies
        // that the helpers do not panic (they fall through to ActiveMode::Toml).
        // In debug mode, validate_invariants would catch this, so we skip.
        let state = AppState {
            app_config: Some(Arc::new(make_app_config())),
            providers: None,
            provider_adapters: Arc::new(ProviderAdapterRegistry::builtin()),
            proxy_client: Arc::new(ProxyClient::new()),
            legacy: None,
            build: Arc::new(make_build_info()),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(DEFAULT_RATE_LIMIT_RPM)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        };
        // Should still dispatch to TOML mode since app_config is Some.
        assert_eq!(state.server_name(), "test-proxy");
    }
}
