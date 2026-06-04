//! The axum wiring for `llm-proxy`.
//!
//! Exposes [`build_router`] which assembles the full HTTP router
//! with middleware, routes, and shared state.

#![deny(missing_docs)]

/// API-layer error type.
pub mod error;
/// Middleware components (dedup, rate limiter, request ID, IP extraction).
pub mod middleware;
/// Route handlers and router composition.
pub mod routes;
/// Graceful shutdown signal.
pub mod shutdown;
/// Shared application state and build metadata.
pub mod state;

pub use error::ApiError;
pub use middleware::{RateLimiter, RequestDeduplicator, RequestIdGenerator};
pub use shutdown::shutdown_signal;
pub use state::{AppState, BuildInfo, ModelRouter};

use axum::Router;

/// Build the application router for the given state.
#[allow(clippy::double_must_use)]
#[must_use = "the Router must be used with a server"]
pub fn build_router(state: AppState) -> Router {
    routes::router(state)
}
