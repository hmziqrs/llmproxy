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
/// Middleware order (outermost to innermost per group):
///
/// **Lightweight routes** (`/health`, `/ready`, `/version`):
/// 1. `TraceLayer` -- logs every request and response, measures latency.
///
/// **API routes** (`/v1/*`):
/// 1. `TraceLayer` -- logs every request and response, measures latency.
/// 2. `TimeoutLayer` -- cancels requests exceeding `config.request_timeout`.
/// 3. `DefaultBodyLimit` -- caps the request body for JSON extractors.
///
/// `TimeoutLayer` is scoped to `/v1/*` only. Lightweight health/readiness
/// endpoints respond instantly and must not be subject to a 408 timeout,
/// which would confuse orchestrators (Kubernetes, load balancers).
///
/// Note: `TimeoutLayer` still applies to streaming SSE responses within the
/// `/v1/*` group. The configured `request_timeout` must be set high enough
/// for long-running LLM streaming responses. Consider exempting streaming
/// routes specifically in a future phase.
pub fn router(state: AppState) -> Router {
    let timeout = state.config.request_timeout;

    // Lightweight routes: tracing only, no timeout or body limit.
    let lightweight = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .layer(TraceLayer::new_for_http());

    // API routes: tracing + timeout + body limit.
    let api_middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    let api = Router::new()
        .route("/v1/messages", post(handle_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .layer(api_middleware);

    Router::new()
        .merge(lightweight)
        .merge(api)
        .fallback(not_found)
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
