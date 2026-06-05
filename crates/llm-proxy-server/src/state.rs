use std::sync::Arc;

use llm_proxy_core::{Config, Counter, FallbackHandler, Metrics};
use llm_proxy_provider::OpenCodeClient;

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
    //
    // In Go the test verifies that resolveRequestedModel returns the primary
    // model plus the "default" fallback chain.  In Rust, the ModelRouter
    // splits this into resolve() + fallback_chain(), so we test both
    // together to capture the same semantics.

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
    //
    // The Go tests verify that when respect_requested_model=true and the
    // requested model exists as a config key, it bypasses scenario routing.
    // In Rust, respect_requested_model is handled at the messages.rs layer.
    // Here we verify that the ModelRouter can look up a model by its literal
    // model ID (e.g. "kimi-k2.6") when it is configured as a scenario key.

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
}

/// Application state shared with every handler.
///
/// Cheap to clone: all fields are `Arc`-wrapped so cloning only bumps
/// reference counts. Required by axum's `State` extractor.
///
/// # Security note
///
/// Manual `Debug` impl is provided to ensure `api_key` in the nested
/// [`Config`] is never leaked through debug formatting (e.g. in error logs).
#[derive(Clone)]
pub struct AppState {
    /// Server configuration.
    pub config: Arc<Config>,
    /// Build identifier reported by `/version`.
    pub build: Arc<BuildInfo>,
    /// HTTP client for upstream providers.
    pub client: Arc<OpenCodeClient>,
    /// Model router for scenario-based model selection.
    pub model_router: Arc<ModelRouter>,
    /// Fallback handler with circuit breaker protection.
    pub fallback_handler: Arc<FallbackHandler>,
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
            .field("config", &self.config) // Config has its own redacting Debug
            .field("build", &self.build)
            .field("client", &self.client)
            .field("model_router", &self.model_router)
            .field("fallback_handler", &self.fallback_handler)
            .field("token_counter", &self.token_counter)
            .field("metrics", &self.metrics)
            .field("rate_limiter", &self.rate_limiter)
            .field("request_dedup", &self.request_dedup)
            .field("request_id_gen", &self.request_id_gen)
            .finish()
    }
}

impl AppState {
    /// Construct a new `AppState` from all components.
    #[must_use]
    pub fn new(
        config: Config,
        build: BuildInfo,
        client: OpenCodeClient,
        fallback_handler: FallbackHandler,
    ) -> Self {
        let config_arc = Arc::new(config);
        Self {
            client: Arc::new(client),
            model_router: Arc::new(ModelRouter::new(Arc::clone(&config_arc))),
            fallback_handler: Arc::new(fallback_handler),
            config: config_arc,
            build: Arc::new(build),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        }
    }
}
