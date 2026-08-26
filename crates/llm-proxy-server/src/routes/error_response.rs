//! Protocol-aware error encoding for route handlers.
//!
//! Defines [`RouteError`] as the single internal error model used by the core
//! pipeline, and [`route_error_response`] to encode it into a client-specific
//! HTTP response. Each route passes its [`ClientProtocol`] so the error shape
//! matches what the client expects (Anthropic JSON envelope, OpenAI error, etc.).
//!
//! # Security: error message sanitization
//!
//! Internal error messages (config details, adapter names, protocol names, etc.)
//! are NEVER sent to clients. Instead, generic messages are used in the HTTP
//! response body, while the actual messages are logged server-side only. This
//! prevents information disclosure about the proxy's internal architecture.
//!
//! Upstream error bodies are sanitized by the provider error layer and further
//! truncated here. For codes that pass through to the client (400/413/429 and
//! client-owned 401/403), the provider-specific message is replaced with a
//! generic proxy message and the real body is logged server-side only, so the
//! upstream's own schema never reaches the client. Provider decode errors are
//! similarly replaced with generic messages to avoid leaking upstream response
//! fragments.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

// ---------------------------------------------------------------------------
// ClientProtocol
// ---------------------------------------------------------------------------

/// Identifies the client-facing protocol for error encoding.
///
/// Each route knows which protocol its callers speak. The error response module
/// uses this to produce the correct JSON envelope.
///
/// Rate limiting, deduplication, routing, and provider failures are represented
/// by [`RouteError`] and encoded according to this protocol selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(super) enum ClientProtocol {
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI Chat Completions API (`/v1/chat/completions`).
    OpenAiChat,
}

// ---------------------------------------------------------------------------
// RouteError
// ---------------------------------------------------------------------------

/// Who owns the credentials used to authenticate with the upstream provider.
///
/// This distinguishes the two operating modes of the proxy and drives how
/// upstream auth/permission failures (401/403) are surfaced to the client:
///
/// - [`AuthOwner::Operator`] (default, managed-key mode): the proxy forwards
///   *its own* configured upstream credentials. An upstream 401/403 therefore
///   means the *operator's* key is expired/revoked/scoped wrong -- an
///   infrastructure problem the client cannot fix -- so it is collapsed to
///   502 Bad Gateway (see [`map_upstream_status`]).
/// - [`AuthOwner::Client`] (`passthrough_auth = true`): the proxy forwards the
///   *client's* own inbound token. An upstream 401/403 is then the client's
///   credential problem and is passed through verbatim so the client can act
///   on it (audit LOW, passthrough-auth-401-collapse).
///
/// Non-auth codes (400/413/429) are always client-owned and pass through
/// regardless of this value; 404/5xx always collapse to 502.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AuthOwner {
    /// The proxy's own (operator) credentials are forwarded (managed-key mode).
    #[default]
    Operator,
    /// The client's own credentials are forwarded (`passthrough_auth`).
    Client,
}

/// Internal error type for the core pipeline.
///
/// Route handlers return `Result<T, RouteError>`. The [`route_error_response`]
/// function encodes this into a protocol-specific HTTP response.
///
/// # Security note
///
/// Some variants carry internal details (Internal, ProviderDecode, Upstream).
/// The error response encoder sanitizes these before sending them to clients.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(super) enum RouteError {
    /// Bad client input (malformed JSON, missing fields).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The requested model is excluded by provider catalog enforcement.
    #[error("model not allowed: {0}")]
    ModelNotAllowed(String),
    /// The requested provider name is not registered.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    /// The route kind is not supported by the named provider.
    #[error("unsupported route: {0}")]
    UnsupportedRoute(String),
    /// The provider name in the URL path contains invalid characters.
    #[error("invalid provider name: {0}")]
    InvalidProviderName(String),
    /// Upstream provider returned an error status.
    #[error("upstream error: {status}")]
    Upstream {
        /// HTTP status code from the upstream (or a synthetic one).
        status: StatusCode,
        /// Sanitized error body.
        body: String,
        /// Whose credentials authenticated with the upstream. Drives whether
        /// auth/permission codes (401/403) pass through to the client
        /// ([`AuthOwner::Client`]) or collapse to 502 ([`AuthOwner::Operator`]).
        auth_owner: AuthOwner,
    },
    /// Upstream provider timed out.
    #[error("upstream timeout: {0}")]
    UpstreamTimeout(String),
    /// Provider adapter failed to decode the upstream response.
    #[error("provider decode error: {0}")]
    ProviderDecode(String),
    /// Internal server error (misconfiguration, missing state).
    ///
    /// **Security note:** The message is for server-side logging only. The error
    /// response encoder discards it and sends a generic `INTERNAL_ERROR_CLIENT_MESSAGE`
    /// to clients. Any future error handling path must NOT format this variant's
    /// message directly into client responses.
    #[error("internal error: {0}")]
    Internal(String),
    /// Client has exceeded its rate limit.
    #[error("rate limit exceeded")]
    RateLimited,
    /// Duplicate request detected by the deduplicator.
    #[error("duplicate request")]
    Conflict,
    /// The requested path does not match any mounted route.
    #[error("not found")]
    NotFound,
    /// The request exceeded the configured server timeout (408).
    ///
    /// Produced by the per-route timeout layer, then normalised into a
    /// protocol-shaped JSON body by the outermost response-normaliser layer
    /// (audit MEDIUM-7). Distinct from [`RouteError::UpstreamTimeout`], which
    /// is a 504 for an upstream call that exceeded its own deadline.
    #[error("request timeout")]
    RequestTimeout,
    /// The request body exceeded the maximum allowed size (413).
    #[error("request body too large")]
    PayloadTooLarge,
    /// The request used an HTTP method the matched route does not allow (405).
    ///
    /// Produced by the router-wide `method_not_allowed_fallback` so that axum's
    /// bare 405 (empty body) is rendered through the same protocol-shaped JSON
    /// envelope as 404/408/413 (audit LOW-27).
    #[error("method not allowed")]
    MethodNotAllowed,
    /// The client did not provide an auth token that a passthrough-auth provider
    /// (`passthrough_auth = true`) requires. Maps to 401.
    #[error("missing client auth token for passthrough provider")]
    Unauthorized,
}

// ---------------------------------------------------------------------------
// JSON envelope types
// ---------------------------------------------------------------------------

/// Anthropic-shaped error body.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AnthropicErrorBody {
    r#type: &'static str,
    error: AnthropicErrorDetail,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AnthropicErrorDetail {
    r#type: String,
    message: String,
}

/// OpenAI Chat-shaped error body.
///
/// Uses a typed struct instead of `serde_json::json!()` for compile-time
/// field validation and consistency with the Anthropic path.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct OpenAiErrorBody {
    error: OpenAiErrorDetail,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct OpenAiErrorDetail {
    message: String,
    r#type: String,
    // Always serializes as `null` to match the OpenAI wire format:
    // `{"error":{"message":"...","type":"...","code":null}}`.
    // Using `serde_json::Value::Null` is more idiomatic than `Option<()>`.
    code: serde_json::Value,
}

// ---------------------------------------------------------------------------
// route_error_response
// ---------------------------------------------------------------------------

/// Encode a [`RouteError`] into a protocol-specific HTTP response.
///
/// For `ClientProtocol::Anthropic`, the response body is an Anthropic-shaped
/// JSON envelope with `{"type":"error","error":{...}}`.
pub(super) fn route_error_response(protocol: ClientProtocol, error: &RouteError) -> Response<Body> {
    match protocol {
        ClientProtocol::Anthropic => anthropic_error_response(error),
        ClientProtocol::OpenAiChat => openai_error_response(error),
    }
}

/// Generic message used for 500 Internal Server Error responses.
/// The actual internal error details are logged server-side only.
pub(crate) const INTERNAL_ERROR_CLIENT_MESSAGE: &str = "internal server error";

/// Generic message used for provider decode error responses.
/// The actual decode error details are logged server-side only.
pub(crate) const PROVIDER_DECODE_CLIENT_MESSAGE: &str = "provider response decode error";

/// Generic message used for upstream 4xx errors that pass through to the
/// client. The provider's own error envelope is logged server-side only and
/// never forwarded, so provider-specific schema (field names, model
/// identifiers, request echoes) does not leak (audit LOW, upstream-400-body-leak).
pub(crate) const UPSTREAM_PASSTHROUGH_CLIENT_MESSAGE: &str = "upstream rejected the request";

/// Generic message used for upstream errors that collapse to 502 (every status
/// that is not in the pass-through allowlist: 404/408/422/5xx, and
/// operator-owned 401/403). Like the pass-through branch, the real (already
/// key/URL-redacted) upstream body is logged server-side only and a generic
/// message is returned, so the provider's own schema never reaches the client
/// envelope or the persisted `ResponseFailed.message` (audit finding:
/// 502-collapse forwarded a URL-only-redacted body carrying provider schema).
pub(crate) const UPSTREAM_COLLAPSE_CLIENT_MESSAGE: &str = "upstream service error";

/// Build an Anthropic-shaped error response.
///
/// Internal error messages are sanitized: `Internal` and `ProviderDecode`
/// variants use generic messages in the response body to prevent information
/// disclosure. The actual messages are available through `RouteError::Display`
/// for server-side logging before this function is called.
fn anthropic_error_response(error: &RouteError) -> Response<Body> {
    let (status, error_type, message) = extract_error_fields(error);

    let body = AnthropicErrorBody {
        r#type: "error",
        error: AnthropicErrorDetail {
            r#type: error_type.to_owned(),
            message,
        },
    };

    // axum::Json already sets Content-Type: application/json in its
    // IntoResponse implementation. The explicit insert below is
    // defense-in-depth to ensure the header is always present.
    let mut response = (status, axum::Json(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// Build an OpenAI Chat-shaped error response.
///
/// The error envelope follows the OpenAI error shape:
/// `{"error":{"message":"...","type":"invalid_request_error","code":null}}`.
///
/// Internal error messages are sanitized: `Internal` and `ProviderDecode`
/// variants use generic messages in the response body to prevent information
/// disclosure.
///
/// # Two-stage error mapping
///
/// 1. [`extract_error_fields`] first converts the [`RouteError`] into a
///    (status, error_type, message) tuple. Upstream errors go through
///    [`map_upstream_status`] which converts most upstream codes to 502.
/// 2. This function then overrides `error_type` for specific status codes
///    to match OpenAI conventions: 500 becomes `"server_error"` and 404
///    becomes `"invalid_request_error"`.
///
/// # Why 502 Bad Gateway for upstream errors?
///
/// When the upstream provider returns 4xx/5xx, the proxy maps most of these
/// to 502 Bad Gateway (except 429 which is passed through). This signals to
/// the client that the failure is between the proxy and the upstream, not a
/// client error. The original upstream status is preserved in server-side
/// logs for debugging.
fn openai_error_response(error: &RouteError) -> Response<Body> {
    let (status, error_type, message) = extract_error_fields(error);
    // OpenAI uses "server_error" for internal errors instead of "api_error".
    let error_type = match status {
        StatusCode::INTERNAL_SERVER_ERROR => "server_error",
        StatusCode::NOT_FOUND => "invalid_request_error",
        _ => error_type,
    };

    let body = OpenAiErrorBody {
        error: OpenAiErrorDetail {
            message,
            r#type: error_type.to_owned(),
            code: serde_json::Value::Null,
        },
    };

    // axum::Json already sets Content-Type: application/json. The explicit
    // insert below is defense-in-depth.
    let mut response = (status, axum::Json(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build an OpenAI-shaped SSE error JSON string with a custom error type.
pub(super) fn openai_stream_error_json_with_type(
    message: &str,
    error_type: &str,
) -> Option<String> {
    let body = OpenAiErrorBody {
        error: OpenAiErrorDetail {
            message: truncate_error_body(message),
            r#type: error_type.to_owned(),
            code: serde_json::Value::Null,
        },
    };
    serde_json::to_string(&body).ok()
}

/// Return the Rust variant name of a [`RouteError`].
///
/// The structured event-log contract documents `ResponseFailed.error_kind` as
/// "from the `RouteError` variant name" (see the `ResponseFailed` doc in
/// `llm-proxy-storage`), so operators querying the event log by failure class
/// match a stable discriminant. This is deliberately distinct from the
/// client-facing protocol error-type string returned by
/// [`extract_error_fields`] (e.g. `"api_error"`, `"not_found_error"`): several
/// unrelated variants share an envelope string, so the variant name is the only
/// value that keeps `UnknownProvider`, `NotFound`, and `RateLimited`
/// separable downstream (audit finding: error_kind not variant name).
///
/// Returns a `&'static str` matching the variant identifier exactly.
pub(crate) fn route_error_variant_name(error: &RouteError) -> &'static str {
    match error {
        RouteError::InvalidRequest(_) => "InvalidRequest",
        RouteError::ModelNotAllowed(_) => "ModelNotAllowed",
        RouteError::UnknownProvider(_) => "UnknownProvider",
        RouteError::UnsupportedRoute(_) => "UnsupportedRoute",
        RouteError::InvalidProviderName(_) => "InvalidProviderName",
        RouteError::Upstream { .. } => "Upstream",
        RouteError::UpstreamTimeout(_) => "UpstreamTimeout",
        RouteError::ProviderDecode(_) => "ProviderDecode",
        RouteError::Internal(_) => "Internal",
        RouteError::RateLimited => "RateLimited",
        RouteError::Conflict => "Conflict",
        RouteError::NotFound => "NotFound",
        RouteError::RequestTimeout => "RequestTimeout",
        RouteError::PayloadTooLarge => "PayloadTooLarge",
        RouteError::MethodNotAllowed => "MethodNotAllowed",
        RouteError::Unauthorized => "Unauthorized",
    }
}

/// Extract the (status, error_type, message) tuple from a RouteError.
///
/// Shared between `anthropic_error_response` and `openai_error_response` to
/// avoid duplicating the match arms. Each caller wraps the tuple in its own
/// JSON envelope.
pub(crate) fn extract_error_fields(error: &RouteError) -> (StatusCode, &'static str, String) {
    match error {
        RouteError::InvalidRequest(msg) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            msg.clone(),
        ),
        RouteError::ModelNotAllowed(model) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("model not allowed: {model}"),
        ),
        RouteError::UnknownProvider(provider) => (
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("unknown provider: {provider}"),
        ),
        RouteError::UnsupportedRoute(msg) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            msg.clone(),
        ),
        RouteError::InvalidProviderName(msg) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("invalid provider name: {msg}"),
        ),
        RouteError::Upstream {
            status,
            body: _body,
            auth_owner,
        } => {
            let mapped = map_upstream_status(*status, *auth_owner);
            // Both branches return a generic message to the client (and to the
            // persisted `ResponseFailed.message`), so the upstream's own schema
            // (field names, model identifiers, request echoes) never reaches
            // the client envelope or the event log. The real upstream body --
            // already key/URL-redacted at construction -- is logged server-side
            // once in `emit_response_failed`, NOT here: this function is also
            // called from `route_error_response` on the render path, so logging
            // here would double-warn every upstream failure.
            let message = if mapped == *status {
                UPSTREAM_PASSTHROUGH_CLIENT_MESSAGE
            } else {
                UPSTREAM_COLLAPSE_CLIENT_MESSAGE
            };
            (mapped, "api_error", truncate_error_body(message))
        }
        RouteError::UpstreamTimeout(_msg) => (
            StatusCode::GATEWAY_TIMEOUT,
            "api_error",
            "upstream request timed out".to_owned(),
        ),
        RouteError::ProviderDecode(_msg) => (
            StatusCode::BAD_GATEWAY,
            "api_error",
            PROVIDER_DECODE_CLIENT_MESSAGE.to_owned(),
        ),
        RouteError::Internal(_msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            INTERNAL_ERROR_CLIENT_MESSAGE.to_owned(),
        ),
        RouteError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate limit exceeded".to_owned(),
        ),
        RouteError::Conflict => (
            StatusCode::CONFLICT,
            "invalid_request_error",
            "duplicate request, please retry".to_owned(),
        ),
        RouteError::NotFound => (
            StatusCode::NOT_FOUND,
            "not_found_error",
            "not found".to_owned(),
        ),
        RouteError::RequestTimeout => (
            StatusCode::REQUEST_TIMEOUT,
            "timeout_error",
            "request timed out".to_owned(),
        ),
        RouteError::PayloadTooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request_error",
            "request body too large".to_owned(),
        ),
        RouteError::MethodNotAllowed => (
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request_error",
            "method not allowed".to_owned(),
        ),
        RouteError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing or invalid client auth token".to_owned(),
        ),
    }
}

/// Map an upstream HTTP status to the status we return to the client.
///
/// Most upstream >= 400 errors become 502 Bad Gateway because they indicate a
/// problem between the proxy and the upstream provider rather than a client
/// error. A conservative allowlist of clearly-client-owned codes is passed
/// through verbatim so the client can act on the failure:
///   - `400 Bad Request` -- the upstream rejected the *content* of a request
///     the proxy faithfully forwarded, which is almost always the client's
///     responsibility.
///   - `413 Payload Too Large` -- the upstream rejected the body size the
///     client sent (mirrors our own `DefaultBodyLimit` 413).
///   - `429 Too Many Requests` -- propagated so clients can implement their
///     own back-off.
///
/// Auth/permission codes (`401 Unauthorized`, `403 Forbidden`) depend on
/// [`AuthOwner`]:
///   - [`AuthOwner::Operator`] (managed-key mode, the default): the proxy
///     forwards *its own* credentials, so an upstream 401/403 almost always
///     means the *operator's* key is expired/revoked/scoped wrong -- an
///     infrastructure problem the client cannot fix. These collapse to 502 so
///     an operator/infrastructure issue is not misattributed to the client.
///   - [`AuthOwner::Client`] (`passthrough_auth`): the proxy forwards the
///     *client's* own token, so a 401/403 is the client's credential problem
///     and passes through verbatim (audit LOW, passthrough-auth-401-collapse).
///
/// Other ambiguous or server-side codes (404, 408, 422, 5xx, etc.) also remain
/// 502: they either hint at proxy/provider configuration (404 model-not-found)
/// or are not unambiguously the client's fault, so collapsing them keeps the
/// client contract uniform and avoids leaking provider-specific schemas. The
/// original upstream status is always preserved in server-side logs for operators.
///
/// Upstream statuses that may indicate a configuration issue (401/403/404 under
/// [`AuthOwner::Operator`]) are logged at warn level so operators can
/// distinguish them in logs.
fn map_upstream_status(upstream: StatusCode, auth_owner: AuthOwner) -> StatusCode {
    match upstream.as_u16() {
        // Always-client-owned codes: forward verbatim.
        400 | 413 | 429 => upstream,
        // Auth/permission codes pass through only when the client's own
        // credentials were forwarded; otherwise collapse to 502 (operator-side).
        401 | 403 if auth_owner == AuthOwner::Client => upstream,
        code => {
            // Auth/permission (401/403 when operator-owned) and not-found (404)
            // statuses usually indicate an operator configuration problem
            // (bad/expired upstream key, unknown model) rather than a transient
            // upstream failure; log them at warn so operators can diagnose.
            if matches!(code, 401 | 403 | 404) {
                tracing::warn!(
                    upstream_status = code,
                    auth_owner = ?auth_owner,
                    "upstream returned a status that may indicate a configuration \
                     issue (auth, forbidden, or not found); mapping to 502 for client"
                );
            } else if code < 400 {
                // Informational (1xx), success (2xx), or redirect (3xx) codes should
                // never reach the error path. Log at debug level for diagnostics.
                tracing::debug!(
                    upstream_status = code,
                    "unexpected 1xx/2xx/3xx upstream status in error path; mapping to 502"
                );
            }
            StatusCode::BAD_GATEWAY
        }
    }
}

/// Maximum length for error messages returned to clients.
const MAX_ERROR_MESSAGE_LEN: usize = 512;

/// Suffix appended when an error body is truncated.
const TRUNCATED_SUFFIX: &str = "...[truncated]";

/// Truncate an error body to a safe length for client responses.
///
/// Delegates to the shared implementation in `llm_proxy_protocol::util`.
pub(crate) fn truncate_with_suffix(s: &str, max_len: usize, suffix: &str) -> String {
    llm_proxy_protocol::util::truncate_with_suffix(s, max_len, suffix)
}

/// Truncate an error body to a safe length for client responses.
///
/// The final string is at most `MAX_ERROR_MESSAGE_LEN` bytes (the suffix is
/// included within this budget).
fn truncate_error_body(body: &str) -> String {
    truncate_with_suffix(body, MAX_ERROR_MESSAGE_LEN, TRUNCATED_SUFFIX)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Anthropic error encoding ----------------------------------------------

    #[test]
    fn anthropic_invalid_request_returns_400() {
        let err = RouteError::InvalidRequest("bad input".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn anthropic_model_not_allowed_returns_400() {
        let err = RouteError::ModelNotAllowed("gpt-99".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn anthropic_upstream_500_returns_502() {
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream error".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn anthropic_upstream_429_returns_429() {
        let err = RouteError::Upstream {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "rate limited".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn anthropic_provider_decode_returns_502() {
        let err = RouteError::ProviderDecode("bad frame with sensitive data".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn anthropic_internal_returns_500() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn anthropic_rate_limited_returns_429() {
        let err = RouteError::RateLimited;
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn anthropic_conflict_returns_409() {
        let err = RouteError::Conflict;
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    // -- Internal error message sanitization -----------------------------------

    #[tokio::test]
    async fn internal_error_message_is_sanitized_in_response_body() {
        let err = RouteError::Internal("secret config detail: /etc/proxy.toml".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, INTERNAL_ERROR_CLIENT_MESSAGE);
        assert!(
            !message.contains("secret"),
            "internal error message must not contain internal details"
        );
    }

    #[tokio::test]
    async fn provider_decode_error_message_is_sanitized_in_response_body() {
        let err = RouteError::ProviderDecode(
            "decode response: upstream returned malformed JSON with api_key=sk-ant-abc123".into(),
        );
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, PROVIDER_DECODE_CLIENT_MESSAGE);
        assert!(
            !message.contains("upstream"),
            "provider decode error must not contain upstream details"
        );
    }

    // -- OpenAI error encoding -------------------------------------------------

    #[test]
    fn openai_invalid_request_returns_400() {
        let err = RouteError::InvalidRequest("bad input".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn openai_model_not_allowed_returns_400() {
        let err = RouteError::ModelNotAllowed("gpt-99".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn openai_upstream_500_returns_502() {
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream error".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn openai_upstream_429_returns_429() {
        let err = RouteError::Upstream {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "rate limited".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn openai_provider_decode_returns_502() {
        let err = RouteError::ProviderDecode("bad frame".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn openai_internal_returns_500() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn openai_rate_limited_returns_429() {
        let err = RouteError::RateLimited;
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn openai_conflict_returns_409() {
        let err = RouteError::Conflict;
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn openai_invalid_request_body_has_correct_structure() {
        let err = RouteError::InvalidRequest("bad input".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert!(json["error"].is_object(), "must have error object");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["message"], "bad input");
        assert!(json["error"]["code"].is_null(), "code must be null");
        // Must NOT have Anthropic-shaped fields.
        assert!(
            json.get("type").is_none() || json["type"].is_null(),
            "must not have Anthropic 'type' field"
        );
    }

    #[tokio::test]
    async fn openai_model_not_allowed_body_has_correct_structure() {
        let err = RouteError::ModelNotAllowed("gpt-99".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("gpt-99")
        );
        assert!(json["error"]["code"].is_null());
    }

    #[tokio::test]
    async fn openai_upstream_500_body_has_correct_structure() {
        // A 500 collapses to 502. The client must see a GENERIC message -- the
        // upstream's own body (which may carry provider-specific schema, model
        // identifiers, or request echoes) is logged server-side only and never
        // reaches the client envelope (audit finding: 502-collapse message leak).
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "model 'gpt-foo' is overloaded: request echoed here".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "api_error");
        assert_eq!(json["error"]["message"], UPSTREAM_COLLAPSE_CLIENT_MESSAGE);
        assert!(json["error"]["code"].is_null());
        // The provider-specific schema/body must NOT reach the client.
        let message = json["error"]["message"].as_str().unwrap();
        assert!(!message.contains("gpt-foo"));
        assert!(!message.contains("overloaded"));
        assert!(!message.contains("echoed"));
    }

    #[tokio::test]
    async fn upstream_passthrough_400_uses_generic_message() {
        // A pass-through code (400) forwards a GENERIC message to the client so
        // the upstream's own schema (field names, model identifiers, request
        // echoes) never leaks. The real upstream body is logged server-side
        // only (audit LOW, upstream-400-body-leak).
        let err = RouteError::Upstream {
            status: StatusCode::BAD_REQUEST,
            body: r#"{"error":{"message":"model 'gpt-foo' does not exist","type":"invalid_request_error"}}"#.into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, UPSTREAM_PASSTHROUGH_CLIENT_MESSAGE);
        // The provider-specific schema/model name must NOT reach the client.
        assert!(!message.contains("gpt-foo"));
        assert!(!message.contains("invalid_request_error"));
    }

    #[tokio::test]
    async fn upstream_passthrough_401_client_uses_generic_message_and_passes_through() {
        // Under passthrough_auth a 401 is client-owned and passes through, but
        // the body is still the generic message (not the provider envelope).
        let err = RouteError::Upstream {
            status: StatusCode::UNAUTHORIZED,
            body: "invalid api key sk-leaked".into(),
            auth_owner: AuthOwner::Client,
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, UPSTREAM_PASSTHROUGH_CLIENT_MESSAGE);
        assert!(!message.contains("sk-leaked"));
    }

    #[tokio::test]
    async fn upstream_401_operator_collapses_to_502() {
        // Managed-key mode: a 401 collapses to 502 (operator's key is bad).
        let err = RouteError::Upstream {
            status: StatusCode::UNAUTHORIZED,
            body: "invalid api key".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn openai_internal_message_is_sanitized() {
        let err = RouteError::Internal("secret config detail: /etc/proxy.toml".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, INTERNAL_ERROR_CLIENT_MESSAGE);
        assert!(
            !message.contains("secret"),
            "internal error message must not contain internal details"
        );
    }

    #[tokio::test]
    async fn openai_provider_decode_message_is_sanitized() {
        let err = RouteError::ProviderDecode(
            "decode response: upstream returned malformed JSON with api_key=sk-ant-abc123".into(),
        );
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, PROVIDER_DECODE_CLIENT_MESSAGE);
    }

    #[tokio::test]
    async fn openai_rate_limited_body_has_correct_structure() {
        let err = RouteError::RateLimited;
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "rate_limit_error");
        assert_eq!(json["error"]["message"], "rate limit exceeded");
        assert!(json["error"]["code"].is_null());
    }

    #[tokio::test]
    async fn openai_conflict_body_has_correct_structure() {
        let err = RouteError::Conflict;
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("duplicate")
        );
        assert!(json["error"]["code"].is_null());
    }

    #[test]
    fn openai_error_has_json_content_type() {
        let err = RouteError::InvalidRequest("test".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, &err);
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header");
        assert_eq!(ct, "application/json");
    }

    // -- MEDIUM-7: timeout / payload-too-large variants -----------------------

    #[test]
    fn request_timeout_returns_408() {
        let response = route_error_response(ClientProtocol::Anthropic, &RouteError::RequestTimeout);
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    }

    #[test]
    fn payload_too_large_returns_413() {
        let response =
            route_error_response(ClientProtocol::Anthropic, &RouteError::PayloadTooLarge);
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn request_timeout_anthropic_body_is_json_envelope() {
        let response = route_error_response(ClientProtocol::Anthropic, &RouteError::RequestTimeout);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "timeout_error");
        assert_eq!(json["error"]["message"], "request timed out");
    }

    #[tokio::test]
    async fn payload_too_large_openai_body_is_json_envelope() {
        let response =
            route_error_response(ClientProtocol::OpenAiChat, &RouteError::PayloadTooLarge);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(json["error"]["code"].is_null());
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("too large")
        );
    }

    // -- LOW-27: MethodNotAllowed ---------------------------------------------

    #[test]
    fn method_not_allowed_returns_405() {
        let response =
            route_error_response(ClientProtocol::Anthropic, &RouteError::MethodNotAllowed);
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn method_not_allowed_anthropic_body_is_json_envelope() {
        let response =
            route_error_response(ClientProtocol::Anthropic, &RouteError::MethodNotAllowed);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["message"], "method not allowed");
    }

    #[tokio::test]
    async fn method_not_allowed_openai_body_is_json_envelope() {
        let response =
            route_error_response(ClientProtocol::OpenAiChat, &RouteError::MethodNotAllowed);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["message"], "method not allowed");
        assert!(json["error"]["code"].is_null());
        assert!(
            json.get("type").is_none() || json["type"].is_null(),
            "OpenAI 405 must not carry the Anthropic 'type' field"
        );
    }

    // -- route_error_variant_name ----------------------------------------------

    // The event-log `ResponseFailed.error_kind` is documented as the RouteError
    // variant name (a stable discriminant), NOT the client-facing protocol
    // error-type string. Pin a representative set so distinct variants that
    // share an envelope string (e.g. UnknownProvider vs NotFound, both
    // "not_found_error" to the client) stay separable downstream
    // (audit finding: error_kind not variant name).

    #[test]
    fn route_error_variant_name_returns_variant_identifier() {
        assert_eq!(
            route_error_variant_name(&RouteError::UnknownProvider("p".into())),
            "UnknownProvider"
        );
        assert_eq!(route_error_variant_name(&RouteError::NotFound), "NotFound");
        assert_eq!(
            route_error_variant_name(&RouteError::RateLimited),
            "RateLimited"
        );
        assert_eq!(
            route_error_variant_name(&RouteError::RequestTimeout),
            "RequestTimeout"
        );
        assert_eq!(
            route_error_variant_name(&RouteError::PayloadTooLarge),
            "PayloadTooLarge"
        );
        assert_eq!(
            route_error_variant_name(&RouteError::MethodNotAllowed),
            "MethodNotAllowed"
        );
        assert_eq!(
            route_error_variant_name(&RouteError::Upstream {
                status: StatusCode::BAD_GATEWAY,
                body: String::new(),
                auth_owner: AuthOwner::Operator,
            }),
            "Upstream"
        );
        // The client-facing type string is a DIFFERENT value (proving the two
        // are not accidentally the same source).
        let (status, client_type, _msg) =
            extract_error_fields(&RouteError::UnknownProvider("p".into()));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_ne!(client_type, "UnknownProvider");
    }

    // -- map_upstream_status ---------------------------------------------------

    // A conservative allowlist of clearly-client-owned upstream codes is
    // forwarded verbatim so clients can act on their own bad request /
    // size / rate-limit failures (audit LOW, upstream 4xx mapping). Auth
    // codes (401/403) collapse to 502 for operator-owned credentials and pass
    // through only for client-owned (passthrough_auth) credentials.

    #[test]
    fn upstream_400_passes_through() {
        assert_eq!(
            map_upstream_status(StatusCode::BAD_REQUEST, AuthOwner::Operator),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn upstream_401_operator_maps_to_502() {
        // In managed-key mode an upstream 401 means the operator's key is bad,
        // not the client's credentials -- collapse to 502 so the client is not
        // misattributed an operator/infrastructure issue.
        assert_eq!(
            map_upstream_status(StatusCode::UNAUTHORIZED, AuthOwner::Operator),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn upstream_401_client_passes_through() {
        // Under passthrough_auth the client's own token is forwarded, so an
        // upstream 401 is the client's credential problem and passes through
        // verbatim (audit LOW, passthrough-auth-401-collapse).
        assert_eq!(
            map_upstream_status(StatusCode::UNAUTHORIZED, AuthOwner::Client),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn upstream_403_operator_maps_to_502() {
        assert_eq!(
            map_upstream_status(StatusCode::FORBIDDEN, AuthOwner::Operator),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn upstream_403_client_passes_through() {
        assert_eq!(
            map_upstream_status(StatusCode::FORBIDDEN, AuthOwner::Client),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn upstream_413_passes_through() {
        assert_eq!(
            map_upstream_status(StatusCode::PAYLOAD_TOO_LARGE, AuthOwner::Operator),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn upstream_429_maps_to_429() {
        assert_eq!(
            map_upstream_status(StatusCode::TOO_MANY_REQUESTS, AuthOwner::Operator),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn upstream_404_maps_to_502() {
        // Ambiguous (model-not-found vs unknown path): kept as 502 to avoid
        // leaking provider-specific schemas; logged at warn server-side. 404 is
        // never client-owned, so it collapses regardless of auth owner.
        assert_eq!(
            map_upstream_status(StatusCode::NOT_FOUND, AuthOwner::Client),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn upstream_500_maps_to_502() {
        assert_eq!(
            map_upstream_status(StatusCode::INTERNAL_SERVER_ERROR, AuthOwner::Operator),
            StatusCode::BAD_GATEWAY
        );
    }

    // -- truncate_error_body ---------------------------------------------------

    #[test]
    fn truncate_short_body_unchanged() {
        let body = "short error";
        assert_eq!(truncate_error_body(body), body);
    }

    #[test]
    fn truncate_long_body() {
        let body = "x".repeat(600);
        let truncated = truncate_error_body(&body);
        assert!(truncated.ends_with("...[truncated]"));
        // The suffix is included within the MAX_ERROR_MESSAGE_LEN budget.
        assert!(
            truncated.len() <= MAX_ERROR_MESSAGE_LEN,
            "truncated body must not exceed MAX_ERROR_MESSAGE_LEN, got {}",
            truncated.len()
        );
    }

    #[test]
    fn truncate_exact_length_is_unchanged() {
        // Exactly at the limit should NOT be truncated.
        let body = "a".repeat(MAX_ERROR_MESSAGE_LEN);
        assert_eq!(truncate_error_body(&body), body);

        // One over the limit should be truncated.
        let body = "a".repeat(MAX_ERROR_MESSAGE_LEN + 1);
        let truncated = truncate_error_body(&body);
        assert!(truncated.ends_with("...[truncated]"));
        assert!(truncated.len() <= MAX_ERROR_MESSAGE_LEN);
    }

    #[test]
    fn truncate_multibyte_at_boundary_does_not_panic() {
        // Japanese characters are 3 bytes each in UTF-8.
        let body = "あ".repeat(200); // 600 bytes, exceeds 512
        let truncated = truncate_error_body(&body);
        assert!(truncated.len() <= MAX_ERROR_MESSAGE_LEN);
        assert!(truncated.ends_with("...[truncated]"));
    }

    // -- Response body content-type --------------------------------------------

    #[test]
    fn anthropic_error_has_json_content_type() {
        let err = RouteError::InvalidRequest("test".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header");
        assert_eq!(ct, "application/json");
    }

    // -- Response body JSON structure -----------------------------------------

    #[tokio::test]
    async fn anthropic_invalid_request_body_has_correct_structure() {
        let err = RouteError::InvalidRequest("bad input".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["message"], "bad input");
    }

    #[tokio::test]
    async fn anthropic_model_not_allowed_body_has_correct_structure() {
        let err = RouteError::ModelNotAllowed("gpt-99".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("gpt-99")
        );
    }

    #[tokio::test]
    async fn anthropic_upstream_500_body_has_correct_structure() {
        // A 500 collapses to 502 with a GENERIC message; the upstream body is
        // logged server-side only and never reaches the client (mirror of the
        // OpenAI test; audit finding: 502-collapse message leak).
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "model 'gpt-foo' is overloaded: request echoed here".into(),
            auth_owner: AuthOwner::Operator,
        };
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "api_error");
        assert_eq!(json["error"]["message"], UPSTREAM_COLLAPSE_CLIENT_MESSAGE);
        let message = json["error"]["message"].as_str().unwrap();
        assert!(!message.contains("gpt-foo"));
        assert!(!message.contains("overloaded"));
        assert!(!message.contains("echoed"));
    }

    #[tokio::test]
    async fn anthropic_internal_body_uses_generic_message() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "api_error");
        assert_eq!(json["error"]["message"], INTERNAL_ERROR_CLIENT_MESSAGE);
    }

    #[tokio::test]
    async fn anthropic_rate_limited_body_has_correct_structure() {
        let err = RouteError::RateLimited;
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "rate_limit_error");
        assert_eq!(json["error"]["message"], "rate limit exceeded");
    }

    #[tokio::test]
    async fn anthropic_conflict_body_has_correct_structure() {
        let err = RouteError::Conflict;
        let response = route_error_response(ClientProtocol::Anthropic, &err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("duplicate")
        );
    }

    // -- ClientProtocol equality -----------------------------------------------

    #[test]
    fn client_protocol_equality() {
        assert_eq!(ClientProtocol::Anthropic, ClientProtocol::Anthropic);
        assert_ne!(ClientProtocol::Anthropic, ClientProtocol::OpenAiChat);
    }

    // -- openai_stream_error_json_with_type (finding 55) -----------------------

    #[test]
    fn stream_error_json_produces_valid_structure() {
        let json_str = openai_stream_error_json_with_type("test error", "server_error");
        assert!(json_str.is_some());
        let json: serde_json::Value = serde_json::from_str(&json_str.unwrap()).unwrap();
        assert_eq!(json["error"]["message"], "test error");
        assert_eq!(json["error"]["type"], "server_error");
        assert!(json["error"]["code"].is_null());
    }

    #[test]
    fn stream_error_json_with_empty_message() {
        let json_str = openai_stream_error_json_with_type("", "api_error");
        assert!(json_str.is_some());
        let json: serde_json::Value = serde_json::from_str(&json_str.unwrap()).unwrap();
        assert_eq!(json["error"]["message"], "");
    }
}
