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
/// Note: RateLimited and Conflict variants extend the plan's original 5-variant
/// RouteError definition (InvalidRequest, UnknownModel, Upstream, ProviderDecode,
/// Internal). These are used by prepare_request for rate limiting and deduplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientProtocol {
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI Chat Completions API (`/v1/chat/completions`).
    #[allow(dead_code)]
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
pub enum RouteError {
    /// Bad client input (malformed JSON, missing fields).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The requested model is not in the routing table.
    #[error("unknown model: {0}")]
    UnknownModel(String),
    /// Upstream provider returned an error status.
    #[error("upstream error: {status}")]
    Upstream {
        /// HTTP status code from the upstream (or a synthetic one).
        status: StatusCode,
        /// Sanitized error body.
        body: String,
    },
    /// Provider adapter failed to decode the upstream response.
    #[error("provider decode error: {0}")]
    ProviderDecode(String),
    /// Internal server error (misconfiguration, missing state).
    #[error("internal error: {0}")]
    Internal(String),
    /// Client has exceeded its rate limit.
    #[error("rate limit exceeded")]
    RateLimited,
    /// Duplicate request detected by the deduplicator.
    #[error("duplicate request")]
    Conflict,
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
const INTERNAL_ERROR_CLIENT_MESSAGE: &str = "internal server error";

/// Generic message used for provider decode error responses.
/// The actual decode error details are logged server-side only.
const PROVIDER_DECODE_CLIENT_MESSAGE: &str = "provider response decode error";

/// Build an Anthropic-shaped error response.
///
/// Internal error messages are sanitized: `Internal` and `ProviderDecode`
/// variants use generic messages in the response body to prevent information
/// disclosure. The actual messages are available through `RouteError::Display`
/// for server-side logging before this function is called.
fn anthropic_error_response(error: RouteError) -> Response<Body> {
    let (status, error_type, message) = match error {
        RouteError::InvalidRequest(msg) => {
            (StatusCode::BAD_REQUEST, "invalid_request_error", msg)
        }
        RouteError::UnknownModel(model) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("unknown model: {model}"),
        ),
        RouteError::Upstream { status, body } => (
            map_upstream_status(status),
            "api_error",
            truncate_error_body(&body),
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
    };

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
/// Note: Phase 9 will fully implement the OpenAI Chat error shape. This stub
/// produces a minimal but valid JSON response so the core pipeline compiles.
#[allow(dead_code)]
fn openai_error_response(error: RouteError) -> Response<Body> {
    let (status, message) = match error {
        RouteError::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, msg),
        RouteError::UnknownModel(model) => (
            StatusCode::BAD_REQUEST,
            format!("unknown model: {model}"),
        ),
        RouteError::Upstream { status, body } => {
            (map_upstream_status(status), truncate_error_body(&body))
        }
        RouteError::ProviderDecode(_msg) => {
            (StatusCode::BAD_GATEWAY, PROVIDER_DECODE_CLIENT_MESSAGE.to_owned())
        }
        RouteError::Internal(_msg) => {
            (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_CLIENT_MESSAGE.to_owned())
        }
        RouteError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_owned(),
        ),
        RouteError::Conflict => (
            StatusCode::CONFLICT,
            "duplicate request, please retry".to_owned(),
        ),
    };

    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "error",
            "code": status.canonical_reason().unwrap_or("error").to_lowercase()
        }
    });

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

/// Map an upstream HTTP status to the status we return to the client.
///
/// Upstream >= 400 errors become 502 Bad Gateway because they indicate a problem
/// between the proxy and the upstream provider, not a client error. The one
/// exception is 429 (rate limit) which we propagate as-is so clients can
/// implement their own back-off strategies.
fn map_upstream_status(upstream: StatusCode) -> StatusCode {
    match upstream.as_u16() {
        429 => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Maximum length for error messages returned to clients.
const MAX_ERROR_MESSAGE_LEN: usize = 512;

/// Suffix appended when an error body is truncated.
const TRUNCATED_SUFFIX: &str = "...[truncated]";
const TRUNCATED_SUFFIX_LEN: usize = TRUNCATED_SUFFIX.len();

/// Truncate an error body to a safe length for client responses.
///
/// The final string is at most `MAX_ERROR_MESSAGE_LEN` bytes (the suffix is
/// included within this budget).
fn truncate_error_body(body: &str) -> String {
    if body.len() <= MAX_ERROR_MESSAGE_LEN {
        body.to_owned()
    } else {
        let max_content = MAX_ERROR_MESSAGE_LEN - TRUNCATED_SUFFIX_LEN;
        let mut end = max_content;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}{}", &body[..end], TRUNCATED_SUFFIX)
    }
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
    fn anthropic_unknown_model_returns_400() {
        let err = RouteError::UnknownModel("gpt-99".into());
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
    async fn anthropic_unknown_model_body_has_correct_structure() {
        let err = RouteError::UnknownModel("gpt-99".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(json["error"]["message"].as_str().unwrap().contains("gpt-99"));
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
        assert!(json["error"]["message"].as_str().unwrap().contains("duplicate"));
    }

    // -- ClientProtocol equality -----------------------------------------------

    #[test]
    fn client_protocol_equality() {
        assert_eq!(ClientProtocol::Anthropic, ClientProtocol::Anthropic);
        assert_ne!(ClientProtocol::Anthropic, ClientProtocol::OpenAiChat);
    }
}
