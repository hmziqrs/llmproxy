//! Protocol-aware error encoding for route handlers.
//!
//! Defines [`RouteError`] as the single internal error model used by the core
//! pipeline, and [`route_error_response`] to encode it into a client-specific
//! HTTP response. Each route passes its [`ClientProtocol`] so the error shape
//! matches what the client expects (Anthropic JSON envelope, OpenAI error, etc.).

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

// ---------------------------------------------------------------------------
// JSON envelope types
// ---------------------------------------------------------------------------

/// Anthropic-shaped error body.
#[derive(Serialize)]
struct AnthropicErrorBody {
    r#type: &'static str,
    error: AnthropicErrorDetail,
}

#[derive(Serialize)]
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

/// Build an Anthropic-shaped error response.
fn anthropic_error_response(error: RouteError) -> Response<Body> {
    let (status, error_type, message): (StatusCode, &str, String) = match &error {
        RouteError::InvalidRequest(msg) => {
            (StatusCode::BAD_REQUEST, "invalid_request_error", msg.clone())
        }
        RouteError::UnknownModel(model) => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("unknown model: {model}"),
        ),
        RouteError::Upstream { status, body } => (
            map_upstream_status(*status),
            "api_error",
            truncate_error_body(body),
        ),
        RouteError::ProviderDecode(msg) => (
            StatusCode::BAD_GATEWAY,
            "api_error",
            msg.clone(),
        ),
        RouteError::Internal(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            msg.clone(),
        ),
    };

    let body = AnthropicErrorBody {
        r#type: "error",
        error: AnthropicErrorDetail {
            r#type: error_type.to_owned(),
            message,
        },
    };

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
    let (status, message) = match &error {
        RouteError::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
        RouteError::UnknownModel(model) => (
            StatusCode::BAD_REQUEST,
            format!("unknown model: {model}"),
        ),
        RouteError::Upstream { status, body } => {
            (map_upstream_status(*status), truncate_error_body(body))
        }
        RouteError::ProviderDecode(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
        RouteError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
    };

    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "error",
            "code": status.canonical_reason().unwrap_or("error").to_lowercase()
        }
    });

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
/// Upstream 4xx errors become 502 Bad Gateway because they indicate a problem
/// between the proxy and the upstream provider, not a client error. The one
/// exception is 429 (rate limit) which we propagate as-is.
fn map_upstream_status(upstream: StatusCode) -> StatusCode {
    match upstream.as_u16() {
        429 => StatusCode::TOO_MANY_REQUESTS,
        400 => StatusCode::BAD_REQUEST,
        401 | 403 => StatusCode::BAD_GATEWAY,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Maximum length for error messages returned to clients.
const MAX_ERROR_MESSAGE_LEN: usize = 512;

/// Truncate an error body to a safe length for client responses.
fn truncate_error_body(body: &str) -> String {
    if body.len() <= MAX_ERROR_MESSAGE_LEN {
        body.to_owned()
    } else {
        let mut end = MAX_ERROR_MESSAGE_LEN;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...[truncated]", &body[..end])
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
        let err = RouteError::ProviderDecode("bad frame".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn anthropic_internal_returns_500() {
        let err = RouteError::Internal("config missing".into());
        let response = route_error_response(ClientProtocol::Anthropic, err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
    fn upstream_400_maps_to_400() {
        assert_eq!(
            map_upstream_status(StatusCode::BAD_REQUEST),
            StatusCode::BAD_REQUEST
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
        assert!(truncated.len() <= MAX_ERROR_MESSAGE_LEN + "...[truncated]".len());
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

    // -- ClientProtocol equality -----------------------------------------------

    #[test]
    fn client_protocol_equality() {
        assert_eq!(ClientProtocol::Anthropic, ClientProtocol::Anthropic);
        assert_ne!(ClientProtocol::Anthropic, ClientProtocol::OpenAiChat);
    }
}
