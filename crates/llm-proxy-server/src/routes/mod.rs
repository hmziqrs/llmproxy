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
mod models;
mod token_count;

use chat::handle_chat_completions;
use health::{health, ready, version};
use messages::handle_messages;
use models::handle_models;
use token_count::count_tokens;

/// Maximum request body size for API routes (32 MiB).
///
/// This limit applies to all `/providers/{provider}/v1/*` routes via
/// `DefaultBodyLimit`. The value is chosen to accommodate large multi-turn
/// conversations with tool-use payloads while protecting against resource
/// exhaustion. Requests exceeding this limit receive a 413 Payload Too Large
/// response before any JSON parsing or upstream forwarding occurs.
///
/// Note: This limit does NOT apply to lightweight health/readiness/version
/// endpoints, which accept no request body.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Maximum body size to drain for 404 fallback responses.
///
/// Deliberately smaller than [`MAX_BODY_BYTES`] because 404 responses are
/// never processed -- the body is only drained to clean up the connection.
/// 1 KiB is enough to read any reasonable probe or small payload while
/// avoiding wasting memory on large accidental uploads to wrong paths.
const NOT_FOUND_BODY_DRAIN_LIMIT: usize = 1024;

/// Build the full router.
///
/// Middleware order (outermost to innermost per group):
///
/// **Lightweight routes** (`/health`, `/ready`, `/version`):
/// 1. `TraceLayer` -- logs every request and response, measures latency.
///
/// **API routes** (`/providers/{provider}/v1/*`):
/// 1. `DefaultBodyLimit` -- caps the request body for JSON extractors (runs
///    first so oversized payloads are rejected before the timeout starts).
/// 2. `TraceLayer` -- logs every request and response, measures latency.
/// 3. `TimeoutLayer` -- cancels requests exceeding `config.request_timeout`.
///
/// `TimeoutLayer` is scoped to provider API routes only. Lightweight health/readiness
/// endpoints respond instantly and must not be subject to a 408 timeout,
/// which would confuse orchestrators (Kubernetes, load balancers).
///
/// For streaming handlers, `TimeoutLayer` covers work through creation of the
/// HTTP response, including the first upstream event. Once Axum returns the
/// streaming response body, this layer no longer times subsequent SSE events.
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
    let api_middleware = ServiceBuilder::new()
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ));

    let api = Router::new()
        .route("/providers/{provider}/v1/messages", post(handle_messages))
        .route(
            "/providers/{provider}/v1/messages/count_tokens",
            post(count_tokens),
        )
        .route(
            "/providers/{provider}/v1/chat/completions",
            post(handle_chat_completions),
        )
        .route("/providers/{provider}/v1/models", get(handle_models))
        .layer(api_middleware);

    Router::new()
        .merge(lightweight)
        .merge(api)
        .fallback(not_found)
        .with_state(state)
}

async fn not_found(req: Request) -> impl IntoResponse {
    // Protocol-aware 404: return OpenAI-shaped errors for provider-scoped chat
    // routes and any path under /v1/chat/ (future OpenAI chat sub-routes), and
    // Anthropic-shaped errors for everything else.
    //
    // Boundary note: the path check covers future routes
    // like /v1/chat/edits. If a non-OpenAI protocol is ever mounted under
    // /v1/chat/, this heuristic must be updated.
    //
    // NOTE: This heuristic is fragile and depends on path-prefix conventions
    // rather than a registered route registry. A more robust approach would
    // be to maintain a route-to-protocol mapping that is consulted by the
    // fallback handler. For now, the prefix-based approach is sufficient
    // because only two protocols are mounted and their paths are disjoint.
    let path = req.uri().path().to_owned();

    // Drain the body to ensure the connection is cleaned up promptly.
    // We attempt to read up to NOT_FOUND_BODY_DRAIN_LIMIT bytes. If the body
    // is larger, the remaining bytes are discarded when the request body is
    // dropped. This is acceptable because: (a) the 404 response will cause
    // the client to close the connection, and (b) HTTP/2 multiplexing does
    // not have the same head-of-line blocking concern as HTTP/1.1 keep-alive.
    drop(axum::body::to_bytes(req.into_body(), NOT_FOUND_BODY_DRAIN_LIMIT).await);

    // Match `/v1/chat/completions` and `/providers/{provider}/v1/chat/completions`
    // more specifically to avoid false positives on unrelated `/v1/chat/` paths.
    // The heuristic checks for the full `chat/completions` segment to reduce
    // the chance of matching future non-OpenAI routes under `/v1/chat/`.
    let is_openai_chat_path = path.contains("/v1/chat/completions")
        || path.contains("/v1/chat/edits");

    if is_openai_chat_path {
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
