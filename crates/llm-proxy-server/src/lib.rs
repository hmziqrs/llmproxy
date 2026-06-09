//! The axum wiring for `llm-proxy`.
//!
//! Exposes [`build_router`] which assembles the full HTTP router
//! with middleware, routes, and shared state.

#![deny(missing_docs)]

/// Runtime provider model catalog cache and refresh service.
pub mod catalog_service;
/// Middleware components (dedup, rate limiter, request ID, IP extraction).
pub mod middleware;
/// Route handlers and router composition.
pub mod routes;
/// Graceful shutdown signal.
pub mod shutdown;
/// Shared application state and build metadata.
pub mod state;

pub use catalog_service::{ModelCatalogService, write_catalog_atomic};
pub use middleware::{RateLimiter, RequestDeduplicator, RequestIdGenerator};
pub use shutdown::shutdown_signal;
pub use state::{AppState, BuildInfo};

use axum::Router;

/// Build the application router for the given state.
///
/// `clippy::double_must_use` is suppressed because both `Router` and this
/// function are annotated with `#[must_use]`. This is intentional: the
/// function must be called (to produce the router) and the router must be
/// used (to start a server). Removing either `must_use` would weaken the
/// compile-time safety net.
#[allow(clippy::double_must_use)]
#[must_use = "the Router must be used with a server"]
pub fn build_router(state: AppState) -> Router {
    routes::router(state)
}
