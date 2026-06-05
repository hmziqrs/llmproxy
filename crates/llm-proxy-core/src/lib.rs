//! Core types shared across all `llm-proxy` crates.
//!
//! Provides:
//!
//! - **Configuration**: the legacy JSON [`Config`] and the new TOML
//!   [`AppConfig`]/[`ProviderConfig`] types with env-var interpolation and
//!   validation.
//! - **Routing**: [`ProviderTarget`] and [`resolve_model_route`] for mapping
//!   client-facing model names to upstream providers.
//! - **Scenario routing**: [`Scenario`], [`ScenarioConfig`], and related types
//!   for request classification.
//! - **Metrics**: the runtime [`Metrics`] collector with counters and latency
//!   histograms.
//! - **Token counting**: [`Counter`] and [`MessageContent`] for usage tracking.
//! - **PID management**: [`PidManager`] for daemon mode.
//! - **Errors**: the crate-level [`CoreError`] enum.
//!
//! # Crate layout
//!
//! ```text
//! config            - Legacy JSON config (oc-go-cc compatible)
//! provider_config   - New TOML provider/app config types
//! model_route       - Model routing resolution
//! env_interpolate   - Shared ${ENV_VAR} interpolation
//! router            - Scenario-based request routing
//! token             - Token counting
//! metrics           - Runtime metrics
//! pid               - PID file management
//! error             - Crate-level errors
//! ```

#![deny(missing_docs)]

/// Configuration loading and types (legacy JSON system).
pub mod config;
/// Crate-level error type.
pub mod error;
/// Shared `${ENV_VAR}` interpolation for config files.
pub mod env_interpolate;
/// Runtime metrics (counters, latency ring-buffer, per-model counts).
pub mod metrics;
/// Model routing: resolve client-facing model names to provider targets.
pub mod model_route;
/// PID file management for daemon mode.
pub mod pid;
/// TOML provider configuration types, parsing, and validation.
pub mod provider_config;
/// Scenario-based request routing for model selection.
pub mod router;
/// Token counting utilities.
pub mod token;

/// Shared test utilities (env-var guards, crate-level test mutex).
#[cfg(test)]
pub mod test_support;

pub use config::{Config, LoggingConfig, ModelConfig, OpenCodeGoConfig, OpenCodeZenConfig};
pub use error::CoreError;
pub use metrics::{Metrics, Snapshot};
pub use model_route::{ModelRouteError, ProviderTarget, resolve_model_route};
pub use pid::PidManager;
pub use provider_config::{
    AppConfig, AuthStyle, ConfigValidationError, ModelRoute, ProviderAdapterConfig,
    ProviderConfig, ProviderFile, ProviderModelConfig, ServerConfig, load_app_config,
    load_provider_config, validate_model_routes, validate_provider_config,
};
pub use router::{
    CircuitBreaker, CircuitState, FallbackHandler, FallbackResult, Scenario, ScenarioConfig,
    ScenarioResult, detect_scenario, get_fallback_chain, is_retryable_error, route_for_streaming,
};
pub use token::{Counter, MessageContent};
