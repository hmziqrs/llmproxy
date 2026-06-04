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

/// Application state shared with every handler.
///
/// Cheap to clone: all fields are `Arc`-wrapped so cloning only bumps
/// reference counts. Required by axum's `State` extractor.
#[derive(Clone, Debug)]
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
