//! Core types shared across all `llm-proxy` crates.
//!
//! Provides the server [`Config`] type, the crate-level [`CoreError`] error
//! enum, the runtime [`Metrics`] collector, and the [`PidManager`] for daemon
//! PID file management.

#![deny(missing_docs)]

/// Configuration loading and types.
pub mod config;
/// Crate-level error type.
pub mod error;
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
