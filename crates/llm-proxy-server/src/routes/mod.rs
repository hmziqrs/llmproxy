use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

use crate::state::AppState;

mod chat;
mod health;

use chat::echo_chat;
use health::{health, ready, version};

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024; // 32 MiB

/// Build the full router.
///
/// Middleware order, from outermost to innermost:
/// 1. `TraceLayer`  — logs every request and response, measures latency.
/// 2. `TimeoutLayer` — cancels requests exceeding `config.request_timeout`.
/// 3. `DefaultBodyLimit` — caps the request body for JSON extractors.
///
/// `ServiceBuilder` makes the *first* `.layer()` the outermost, so the
/// layers are listed here in the same outer-to-inner order. `TraceLayer`
/// must be outermost for it to observe requests later layers reject
/// (timeouts, oversized bodies).
pub fn router(state: AppState) -> Router {
    let timeout = state.config.request_timeout;
    let middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/v1/chat/completions", post(echo_chat))
        .fallback(not_found)
        .layer(middleware)
        .with_state(state)
}

async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "not found")
}
