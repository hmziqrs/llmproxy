//! Core types shared across all `llm-proxy` crates.
//!
//! Provides the server [`Config`] type, the crate-level [`CoreError`] error
//! enum, and the runtime [`Metrics`] collector.

#![deny(missing_docs)]

/// Configuration loading and types.
pub mod config;
/// Crate-level error type.
pub mod error;
/// Runtime metrics (counters, latency ring-buffer, per-model counts).
pub mod metrics;
/// Scenario-based request routing for model selection.
pub mod router;
/// Token counting utilities.
pub mod token;

pub use config::{Config, LoggingConfig, ModelConfig, OpenCodeGoConfig, OpenCodeZenConfig};
pub use error::CoreError;
pub use metrics::{Metrics, Snapshot};
pub use router::{
    CircuitBreaker, CircuitState, FallbackHandler, FallbackResult, Scenario, ScenarioConfig,
    ScenarioResult, detect_scenario, get_fallback_chain, is_retryable_error, route_for_streaming,
};
pub use token::{Counter, MessageContent};
