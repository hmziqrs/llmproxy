//! Scenario-based request routing, circuit breaker, and model fallback handling.
//!
//! Detects which model to route to based on message content and token count,
//! following a priority ordering: long context > complex > think > background > default.
//! Once a model is chosen, [`fallback::FallbackHandler`] walks the fallback chain
//! while respecting per-model circuit breakers.

pub mod fallback;
pub mod scenarios;

pub use fallback::{
    CircuitBreaker, CircuitState, FallbackHandler, FallbackResult, get_fallback_chain,
    is_retryable_error,
};
pub use scenarios::{
    Scenario, ScenarioConfig, ScenarioResult, detect_scenario, route_for_streaming,
};
