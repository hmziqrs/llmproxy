use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

use crate::state::AppState;

mod health;
mod messages;
mod token_count;

use health::{health, ready, version};
use messages::handle_messages;
use token_count::count_tokens;

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
///
/// Note: `TimeoutLayer` applies to the entire response lifetime, including
/// streaming. The configured `request_timeout` must be set high enough for
/// long-running LLM streaming responses (the default is 60s, which may be
/// too aggressive for streaming). Consider exempting streaming routes or
/// using a per-route timeout approach in future phases.
///
/// Note: The `TimeoutLayer` also applies globally to lightweight endpoints
/// (`/health`, `/ready`, `/version`). While harmless (they respond instantly),
/// a 408 timeout on a health check is technically wrong. Consider applying
/// `TimeoutLayer` only to `/v1/*` routes using a nested Router with per-route
/// middleware.
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
        .route("/v1/messages", post(handle_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .fallback(not_found)
        .layer(middleware)
        .with_state(state)
}

async fn not_found() -> impl IntoResponse {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "not_found_error",
            "message": "not found"
        }
    });
    (StatusCode::NOT_FOUND, axum::Json(body))
}
