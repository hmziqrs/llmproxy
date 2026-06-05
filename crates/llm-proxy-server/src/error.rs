use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

/// API-layer error type. Implements `IntoResponse` so handlers can
/// return `Result<T, ApiError>`.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Bad client input.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Rate limited.
    #[error("rate limit exceeded: {0}")]
    RateLimited(String),
    /// Upstream provider error.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// Upstream provider timed out.
    #[error("upstream timeout: {0}")]
    UpstreamTimeout(String),
    /// Duplicate request.
    #[error("duplicate request: {0}")]
    Duplicate(String),
    /// Missing or invalid authentication.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// Anything else.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Wraps an [`ApiError`] with a request ID for inclusion in error responses.
#[derive(Debug)]
pub struct ApiErrorWithRequestId {
    /// The underlying error.
    pub error: ApiError,
    /// The request ID to include in the response header.
    pub request_id: String,
}

impl std::fmt::Display for ApiErrorWithRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for ApiErrorWithRequestId {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

impl IntoResponse for ApiErrorWithRequestId {
    fn into_response(self) -> Response {
        let (status, body) = self.error.to_anthropic_response();
        let mut response = (status, body).into_response();
        // Explicitly ensure Content-Type is set, even if the inner representation
        // changes in future refactoring.
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        response.headers_mut().insert(
            "x-request-id",
            self.request_id.parse().unwrap_or_else(|_| HeaderValue::from_static("unknown")),
        );
        response
    }
}

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

impl ApiError {
    /// Convert to an Anthropic-format error JSON and HTTP status code.
    fn to_anthropic_response(&self) -> (StatusCode, Json<AnthropicErrorBody>) {
        match self {
            Self::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "invalid_request_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
            Self::RateLimited(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "rate_limit_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
            Self::Upstream(msg) => (
                StatusCode::BAD_GATEWAY,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "api_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
            Self::UpstreamTimeout(msg) => (
                StatusCode::GATEWAY_TIMEOUT,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "api_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
            Self::Duplicate(msg) => (
                StatusCode::CONFLICT,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "invalid_request_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
            Self::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "authentication_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
            Self::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AnthropicErrorBody {
                    r#type: "error",
                    error: AnthropicErrorDetail {
                        r#type: "api_error".to_owned(),
                        message: msg.clone(),
                    },
                }),
            ),
        }
    }

    /// Wrap this error with a request ID so the response includes an
    /// `x-request-id` header.
    pub fn with_request_id(self, request_id: String) -> ApiErrorWithRequestId {
        ApiErrorWithRequestId {
            error: self,
            request_id,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = self.to_anthropic_response();
        let mut response = (status, body).into_response();
        // Explicitly ensure Content-Type is set for resilience against future refactoring.
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        response
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_maps_to_401() {
        let err = ApiError::Unauthorized("missing x-api-key".to_owned());
        let (status, body) = err.to_anthropic_response();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body.error.r#type, "authentication_error");
        assert_eq!(body.error.message, "missing x-api-key");
    }

    #[test]
    fn unauthorized_into_response() {
        let err = ApiError::Unauthorized("bad key".to_owned());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn all_variants_into_response() {
        // Smoke test: every variant can produce a response.
        let _ = ApiError::BadRequest("test".into()).into_response();
        let _ = ApiError::RateLimited("test".into()).into_response();
        let _ = ApiError::Upstream("test".into()).into_response();
        let _ = ApiError::UpstreamTimeout("test".into()).into_response();
        let _ = ApiError::Duplicate("test".into()).into_response();
        let _ = ApiError::Unauthorized("test".into()).into_response();
        let _ = ApiError::Internal("test".into()).into_response();
    }
}
