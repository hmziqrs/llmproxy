use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

use crate::state::AppState;

mod chat;
mod core_pipeline;
mod error_response;
mod health;
mod messages;
mod token_count;

use chat::handle_chat_completions;
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
/// 1. `DefaultBodyLimit` -- caps the request body for JSON extractors (runs
///    first so oversized payloads are rejected before the timeout starts).
/// 2. `TraceLayer` -- logs every request and response, measures latency.
/// 3. `TimeoutLayer` -- cancels requests exceeding `config.request_timeout`.
///
/// `TimeoutLayer` is scoped to `/v1/*` only. Lightweight health/readiness
/// endpoints respond instantly and must not be subject to a 408 timeout,
/// which would confuse orchestrators (Kubernetes, load balancers).
///
/// Note: `TimeoutLayer` still applies to streaming SSE responses within the
/// `/v1/*` group. The configured `request_timeout` must be set high enough
/// for long-running LLM streaming responses. Exempting streaming routes
/// specifically (e.g. via per-route middleware or a streaming-aware timeout
/// that only covers the request-body phase) is deferred to a future phase.
pub fn router(state: AppState) -> Router {
    let timeout = state.request_timeout();

    // Lightweight routes: tracing only, no timeout or body limit.
    let lightweight = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .layer(TraceLayer::new_for_http());

    // API routes: body limit + tracing + timeout.
    //
    // ServiceBuilder applies layers in reverse order (innermost first), so
    // listing DefaultBodyLimit first makes it the outermost layer: the body
    // size check runs immediately before the timeout starts ticking. This
    // ensures a very large upload on a slow connection gets a 413 Payload Too
    // Large response rather than a 408 Request Timeout.
    //
    // NOTE(phase-12): The TimeoutLayer fires on long-running SSE streams,
    // producing a 408 mid-stream. A proper fix requires either exempting
    // streaming routes from this timeout (per-route middleware) or using a
    // streaming-aware timeout that only covers the request-body/first-byte
    // phase and disables itself once SSE streaming begins. See audit issue
    // in core_pipeline.rs for details.
    let api_middleware = ServiceBuilder::new()
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ));

    let api = Router::new()
        .route("/v1/messages", post(handle_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .layer(api_middleware);

    Router::new()
        .merge(lightweight)
        .merge(api)
        .fallback(not_found)
        .with_state(state)
}

async fn not_found(req: Request) -> impl IntoResponse {
    // Protocol-aware 404: return OpenAI-shaped errors for the /v1/chat/completions
    // route and any path under /v1/chat/ (future OpenAI chat sub-routes), and
    // Anthropic-shaped errors for everything else.
    //
    // Boundary note: the starts_with("/v1/chat/") check covers future routes
    // like /v1/chat/edits. If a non-OpenAI protocol is ever mounted under
    // /v1/chat/, this heuristic must be updated.
    let path = req.uri().path().to_owned();

    // Drain the body to ensure the connection is cleaned up promptly.
    // POST requests to nonexistent endpoints may carry non-trivial bodies.
    let _body = axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await;

    if path == "/v1/chat/completions" || path.starts_with("/v1/chat/") {
        error_response::route_error_response(
            error_response::ClientProtocol::OpenAiChat,
            error_response::RouteError::NotFound,
        )
    } else {
        error_response::route_error_response(
            error_response::ClientProtocol::Anthropic,
            error_response::RouteError::NotFound,
        )
    }
}
