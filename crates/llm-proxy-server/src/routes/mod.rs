use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer, cors::CorsLayer, set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer, trace::TraceLayer,
};
use tracing::info_span;

use crate::middleware::{RequestId, RequestIdGenerator};
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
    // Clone the shared request-id generator before `state` is moved into the
    // router. The outermost `inject_request_id` layer generates one id per
    // request and stamps it into extensions so the tracing span, the handler,
    // and the `x-request-id` header all share a single id (audit MEDIUM-1).
    let id_gen = state.request_id_gen.clone();

    // Span factory shared by both routers: it reads the id injected by the
    // outermost layer so every request span carries `request_id`, matching the
    // id that later appears in handler log events and the response header.
    let trace = TraceLayer::new_for_http().make_span_with(make_request_span);

    // Lightweight routes: request-id injection + tracing only.
    let lightweight = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .layer(
            ServiceBuilder::new()
                .layer(from_fn_with_state(id_gen.clone(), inject_request_id))
                .layer(trace.clone()),
        );

    // API routes: request-id injection, error normalisation, body limit,
    // tracing, timeout.
    //
    // Layer order (outermost first):
    //   1. `inject_request_id` -- generates the id and stamps it into extensions
    //      so the span (step 4) and the handler see the same id.
    //   2. `normalize_error_responses` -- rewrites the 408 (timeout) and 413
    //      (body-limit) rejections emitted by layers below into protocol-shaped
    //      JSON envelopes carrying `x-request-id` (audit MEDIUM-7). It runs
    //      outside the body-limit/timeout layers so their rejections pass
    //      through it on the way out.
    //   3. `DefaultBodyLimit` -- caps the body the JSON extractors can buffer.
    //   4. `TraceLayer` -- opens the request span (carrying the request id).
    //   5. `TimeoutLayer` -- cancels handler work exceeding `request_timeout`.
    //
    // Because the timeout and body-limit layers emit non-JSON, header-less
    // rejections, step 2 normalises their final responses into the same
    // Anthropic/OpenAI-shaped JSON schema every handler uses.
    let api_middleware = ServiceBuilder::new()
        .layer(from_fn_with_state(id_gen, inject_request_id))
        .layer(from_fn(normalize_error_responses))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(trace)
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

    let router = Router::new()
        .merge(lightweight)
        .merge(api)
        .fallback(not_found)
        // Protocol-aware 405: shape method-not-allowed responses through the
        // same error envelope as 404/408/413 instead of axum's empty body
        // (audit LOW-27).
        .method_not_allowed_fallback(method_not_allowed);

    // Config-gated CORS (audit LOW-12). A `CorsLayer` is installed only when
    // the operator explicitly lists cross-origin callers, and then only for
    // those exact origins. This replaces a former `CorsLayer::very_permissive()`
    // that admitted every origin -- the opposite of the finding's intent. The
    // default (`server.allowed_origins` unset) applies no CORS layer, so
    // browsers enforce a same-origin policy and this server-to-server proxy is
    // not unexpectedly reachable from arbitrary web origins.
    let router = match cors_layer_for(state.allowed_origins()) {
        Some(cors) => router.layer(cors),
        None => router,
    };

    router
        // Global, outermost middleware (audit LOW-11, LOW-12) -- applied to the
        // merged router so they cover every route and both fallbacks. CORS sits
        // just inside these so a preflight still receives the headers below and
        // panic protection.
        //   1. `SetResponseHeaderLayer` -- `X-Content-Type-Options: nosniff`
        //      on every response so JSON error bodies are not MIME-sniffed
        //      into executable types (LOW-12).
        //   2. `CatchPanicLayer` -- outermost (LOW-11): converts a panic in
        //      any inner layer or handler into a 500 instead of dropping the
        //      connection (which would otherwise present as a reset to the
        //      client and bypass `TraceLayer`'s response-span logging).
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(CatchPanicLayer::new())
        .with_state(state)
}

/// Build a restrictive `CorsLayer` for an explicit allow-origin list, or return
/// `None` when cross-origin access is disabled (audit LOW-12).
///
/// `None` (no origins configured, or every entry fails to parse as a header
/// value) means "install no CORS layer": browsers then enforce a same-origin
/// default, which is the safe baseline for a server-to-server proxy. When
/// origins are configured the layer echoes `Access-Control-Allow-Origin` only
/// for those exact origins and answers preflight with the proxy's actual
/// methods plus the request headers the LLM SDKs it fronts commonly send.
fn cors_layer_for(origins: Option<&[String]>) -> Option<CorsLayer> {
    let origins = origins?;
    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|origin| match origin.parse::<HeaderValue>() {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(%error, origin, "skipping invalid server.allowed_origins entry");
                None
            }
        })
        .collect();
    if parsed.is_empty() {
        return None;
    }
    // The methods the proxy actually serves; OPTIONS is covered by the CORS
    // preflight the layer itself synthesizes, but is listed so the preflight
    // response advertises it as permitted.
    let methods = [Method::GET, Method::POST, Method::OPTIONS];
    // Request headers the LLM SDKs the proxy fronts commonly send. Kept to a
    // curated set rather than `Any` so the policy stays restrictive.
    let headers = [
        header::CONTENT_TYPE,
        header::AUTHORIZATION,
        header::ACCEPT,
        header::USER_AGENT,
        HeaderName::from_static("x-api-key"),
        HeaderName::from_static("anthropic-version"),
        HeaderName::from_static("anthropic-beta"),
        HeaderName::from_static("x-goog-api-key"),
        HeaderName::from_static("x-request-id"),
    ];
    Some(
        CorsLayer::new()
            .allow_origin(parsed)
            .allow_methods(methods)
            .allow_headers(headers),
    )
}

/// Outermost middleware: mint one request id per request and store it in
/// extensions as a [`RequestId`].
///
/// Running this before `TraceLayer` lets [`make_request_span`] stamp the same
/// id onto the span that the handler later emits in events and attaches to the
/// `x-request-id` header, restoring end-to-end correlation (audit MEDIUM-1).
async fn inject_request_id(
    State(id_gen): State<Arc<RequestIdGenerator>>,
    mut req: Request,
    next: Next,
) -> Response {
    let id = id_gen.next_id();
    req.extensions_mut().insert(RequestId(id));
    next.run(req).await
}

/// Build the per-request tracing span, carrying the injected request id.
///
/// `make_span_with` runs after [`inject_request_id`] has stamped the id into
/// extensions, so the span records the same id that flows to the handler. If
/// the id is somehow absent (e.g. a request that bypassed the injection
/// layer), the span still opens with an empty id rather than panicking.
///
/// Only the request **path** is recorded, never the full URI (audit GAP-LOW-3).
/// Several LLM SDKs fall back to authenticating via `?api_key=`/`?key=` query
/// parameters; logging the full URI would leak those secrets into the tracing
/// span and any downstream log sink. `req.uri().path()` strips the query.
fn make_request_span(req: &Request) -> tracing::Span {
    let id = req.extensions().get::<RequestId>();
    info_span!(
        "request",
        request_id = %id.cloned().unwrap_or_else(|| RequestId(String::new())),
        method = %req.method(),
        path = %req.uri().path(),
    )
}

/// Normalise the non-JSON error bodies emitted by the timeout and body-limit
/// layers into protocol-shaped JSON (audit MEDIUM-7).
///
/// `TimeoutLayer` returns a bare 408 with an empty body, and `DefaultBodyLimit`
/// surfaces a 413 as a plain-text `BytesRejection` from the extractors. Neither
/// carries the `x-request-id` header (that is attached inside the handlers).
/// This layer intercepts both statuses, rebuilds them via
/// [`error_response::route_error_response`] with the correct protocol envelope,
/// and stamps the `x-request-id` header from extensions so the 408/413 paths
/// match the schema every handler returns.
async fn normalize_error_responses(req: Request, next: Next) -> Response {
    // Capture everything we need before the request is consumed by `next`.
    let protocol = protocol_for_path(req.uri().path());
    let request_id = req.extensions().get::<RequestId>().cloned();
    let response = next.run(req).await;
    let status = response.status();

    if status != StatusCode::REQUEST_TIMEOUT && status != StatusCode::PAYLOAD_TOO_LARGE {
        return response;
    }

    // Drain the (empty or small) layer-produced body so the connection is not
    // left half-written; the replacement body is what the client sees.
    if let Err(e) = axum::body::to_bytes(response.into_body(), NOT_FOUND_BODY_DRAIN_LIMIT).await {
        tracing::trace!(error = %e, "draining layer rejection body failed");
    }

    let error = if status == StatusCode::REQUEST_TIMEOUT {
        error_response::RouteError::RequestTimeout
    } else {
        error_response::RouteError::PayloadTooLarge
    };

    let mut rewritten = error_response::route_error_response(protocol, error);
    if let Some(id) = request_id {
        if let Ok(value) = id.0.parse() {
            rewritten.headers_mut().insert("x-request-id", value);
        }
    }
    rewritten
}

/// Select the client protocol for a request path, reusing the same prefix
/// heuristic as [`not_found`].
///
/// The timeout / body-limit layers sit above routing, so on rejection they
/// cannot know which protocol's envelope to render. This helper mirrors the
/// 404 heuristic (`/v1/chat/...` -> OpenAI, otherwise Anthropic) so the
/// normalised 408/413 bodies stay consistent with the handler the request
/// *would* have reached.
fn protocol_for_path(path: &str) -> error_response::ClientProtocol {
    let is_openai_chat_path =
        path.contains("/v1/chat/completions") || path.contains("/v1/chat/edits");
    if is_openai_chat_path {
        error_response::ClientProtocol::OpenAiChat
    } else {
        error_response::ClientProtocol::Anthropic
    }
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
    if let Err(e) = axum::body::to_bytes(req.into_body(), NOT_FOUND_BODY_DRAIN_LIMIT).await {
        tracing::trace!(error = %e, "body drain in 404 handler failed");
    }

    let protocol = protocol_for_path(&path);
    error_response::route_error_response(protocol, error_response::RouteError::NotFound)
}

/// Protocol-aware 405 Method Not Allowed fallback (audit LOW-27).
///
/// A request that hits a registered path with a disallowed method (e.g. GET to
/// a POST-only route) does **not** reach [`not_found`] -- axum's per-route
/// `MethodRouter` returns a stock 405 with an empty body. Without this
/// fallback the client receives an inconsistent, non-protocol-shaped response.
///
/// This handler mirrors [`not_found`]: it drains the body, selects the client
/// protocol via the same path-prefix heuristic, and renders a
/// protocol-shaped 405 envelope. Axum still sets the `Allow` header
/// automatically unless the response sets it, which it does not, so the
/// header is preserved.
///
/// Delegates to [`error_response::route_error_response`] with
/// [`error_response::RouteError::MethodNotAllowed`] so the 405 body is produced
/// by the single shared encoder every other status code uses (audit LOW-27),
/// rather than a bespoke inline envelope.
async fn method_not_allowed(req: Request) -> impl IntoResponse {
    let path = req.uri().path().to_owned();

    // Drain the body for the same connection-cleanup reason as [`not_found`].
    if let Err(e) = axum::body::to_bytes(req.into_body(), NOT_FOUND_BODY_DRAIN_LIMIT).await {
        tracing::trace!(error = %e, "body drain in 405 handler failed");
    }

    let protocol = protocol_for_path(&path);
    error_response::route_error_response(protocol, error_response::RouteError::MethodNotAllowed)
}

#[cfg(test)]
mod tests {
    use super::cors_layer_for;

    #[test]
    fn cors_layer_absent_when_origins_unset() {
        assert!(cors_layer_for(None).is_none());
    }

    #[test]
    fn cors_layer_absent_when_origins_empty() {
        let empty: Vec<String> = Vec::new();
        assert!(cors_layer_for(Some(&empty)).is_none());
    }

    #[test]
    fn cors_layer_absent_when_every_origin_is_invalid() {
        // Newlines and NUL are not legal header-value bytes, so every entry is
        // skipped and no layer is installed (audit LOW-12).
        let bad = vec!["bad\norigin".to_owned(), "\0".to_owned()];
        assert!(cors_layer_for(Some(&bad)).is_none());
    }

    #[test]
    fn cors_layer_present_when_origin_configured() {
        let origins = vec!["https://app.example.com".to_owned()];
        assert!(cors_layer_for(Some(&origins)).is_some());
    }

    #[test]
    fn cors_layer_keeps_valid_origins_while_skipping_invalid() {
        let origins = vec![
            "bad\norigin".to_owned(),
            "https://app.example.com".to_owned(),
        ];
        assert!(cors_layer_for(Some(&origins)).is_some());
    }
}
