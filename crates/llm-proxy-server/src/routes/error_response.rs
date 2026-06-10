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
//! truncated here. Provider decode errors are similarly replaced with generic
//! messages to avoid leaking upstream response fragments.

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
pub enum ClientProtocol {
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI Chat Completions API (`/v1/chat/completions`).
    OpenAiChat,
}

// ---------------------------------------------------------------------------
// RouteError
// ---------------------------------------------------------------------------

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
pub enum RouteError {
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
pub fn route_error_response(protocol: ClientProtocol, error: RouteError) -> Response<Body> {
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

/// Build an Anthropic-shaped error response.
///
/// Internal error messages are sanitized: `Internal` and `ProviderDecode`
/// variants use generic messages in the response body to prevent information
/// disclosure. The actual messages are available through `RouteError::Display`
/// for server-side logging before this function is called.
fn anthropic_error_response(error: RouteError) -> Response<Body> {
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
fn openai_error_response(error: RouteError) -> Response<Body> {
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

/// Build an OpenAI-shaped SSE error JSON string for in-band stream errors.
///
/// Defaults to `"api_error"` as the error type. For other error types, use
/// [`openai_stream_error_json_with_type`] directly.
///
/// Reuses the same typed structs (`OpenAiErrorBody`, `OpenAiErrorDetail`) as
/// the HTTP error path so both paths go through the same compile-time-validated
/// serialization.
#[allow(dead_code)]
pub fn openai_stream_error_json(message: &str) -> Option<String> {
    openai_stream_error_json_with_type(message, "api_error")
}

/// Build an OpenAI-shaped SSE error JSON string with a custom error type.
pub fn openai_stream_error_json_with_type(message: &str, error_type: &str) -> Option<String> {
    let body = OpenAiErrorBody {
        error: OpenAiErrorDetail {
            message: truncate_error_body(message),
            r#type: error_type.to_owned(),
            code: serde_json::Value::Null,
        },
    };
    serde_json::to_string(&body).ok()
}

/// Extract the (status, error_type, message) tuple from a RouteError.
///
/// Shared between `anthropic_error_response` and `openai_error_response` to
/// avoid duplicating the match arms. Each caller wraps the tuple in its own
/// JSON envelope.
fn extract_error_fields(error: RouteError) -> (StatusCode, &'static str, String) {
    match error {
        RouteError::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, "invalid_request_error", msg),
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
        RouteError::UnsupportedRoute(msg) => {
            (StatusCode::BAD_REQUEST, "invalid_request_error", msg)
        }
        RouteError::InvalidProviderName(msg) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("invalid provider name: {msg}"),
        ),
        RouteError::Upstream { status, body } => (
            map_upstream_status(status),
            "api_error",
            truncate_error_body(&body),
        ),
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
    }
}

/// Map an upstream HTTP status to the status we return to the client.
///
/// Upstream >= 400 errors become 502 Bad Gateway because they indicate a problem
/// between the proxy and the upstream provider, not a client error. The one
/// exception is 429 (rate limit) which we propagate as-is so clients can
/// implement their own back-off strategies.
///
/// Specific upstream errors (401, 403) are logged at warn level with the original
/// status so operators can distinguish "upstream auth failure" from "upstream server
/// crash" in logs, even though all are mapped to 502 for the client.
fn map_upstream_status(upstream: StatusCode) -> StatusCode {
    match upstream.as_u16() {
        429 => StatusCode::TOO_MANY_REQUESTS,
        code => {
            // Log specific upstream statuses that may indicate configuration issues
            // rather than transient upstream failures, so operators can diagnose them.
            if matches!(code, 401 | 403 | 404) {
                tracing::warn!(
                    upstream_status = code,
                    "upstream returned a status that may indicate a configuration issue \
                     (auth failure, forbidden, or not found); mapping to 502 for client"
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn anthropic_model_not_allowed_returns_400() {
        let err = RouteError::ModelNotAllowed("gpt-99".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn anthropic_upstream_500_returns_502() {
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream error".into(),
        };
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn anthropic_upstream_429_returns_429() {
        let err = RouteError::Upstream {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "rate limited".into(),
        };
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn anthropic_provider_decode_returns_502() {
        let err = RouteError::ProviderDecode("bad frame with sensitive data".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn anthropic_internal_returns_500() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn anthropic_rate_limited_returns_429() {
        let err = RouteError::RateLimited;
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn anthropic_conflict_returns_409() {
        let err = RouteError::Conflict;
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    // -- Internal error message sanitization -----------------------------------

    #[tokio::test]
    async fn internal_error_message_is_sanitized_in_response_body() {
        let err = RouteError::Internal("secret config detail: /etc/proxy.toml".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn openai_model_not_allowed_returns_400() {
        let err = RouteError::ModelNotAllowed("gpt-99".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn openai_upstream_500_returns_502() {
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream error".into(),
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn openai_upstream_429_returns_429() {
        let err = RouteError::Upstream {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "rate limited".into(),
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn openai_provider_decode_returns_502() {
        let err = RouteError::ProviderDecode("bad frame".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn openai_internal_returns_500() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn openai_rate_limited_returns_429() {
        let err = RouteError::RateLimited;
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn openai_conflict_returns_409() {
        let err = RouteError::Conflict;
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn openai_invalid_request_body_has_correct_structure() {
        let err = RouteError::InvalidRequest("bad input".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
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
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
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
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream error".into(),
        };
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["error"]["type"], "api_error");
        assert_eq!(json["error"]["message"], "upstream error");
        assert!(json["error"]["code"].is_null());
    }

    #[tokio::test]
    async fn openai_internal_message_is_sanitized() {
        let err = RouteError::Internal("secret config detail: /etc/proxy.toml".into());
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
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
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
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
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
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
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
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
        let response = route_error_response(ClientProtocol::OpenAiChat, err);
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header");
        assert_eq!(ct, "application/json");
    }

    // -- map_upstream_status ---------------------------------------------------

    #[test]
    fn upstream_400_maps_to_502() {
        assert_eq!(
            map_upstream_status(StatusCode::BAD_REQUEST),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn upstream_401_maps_to_502() {
        assert_eq!(
            map_upstream_status(StatusCode::UNAUTHORIZED),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn upstream_429_maps_to_429() {
        assert_eq!(
            map_upstream_status(StatusCode::TOO_MANY_REQUESTS),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn upstream_500_maps_to_502() {
        assert_eq!(
            map_upstream_status(StatusCode::INTERNAL_SERVER_ERROR),
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let err = RouteError::Upstream {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream error".into(),
        };
        let response = route_error_response(ClientProtocol::Anthropic, err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "api_error");
        assert_eq!(json["error"]["message"], "upstream error");
    }

    #[tokio::test]
    async fn anthropic_internal_body_uses_generic_message() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
        let response = route_error_response(ClientProtocol::Anthropic, err);
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
}
