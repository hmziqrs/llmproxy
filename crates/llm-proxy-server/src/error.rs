use axum::{
    Json,
    http::StatusCode,
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
    /// Duplicate request.
    #[error("duplicate request: {0}")]
    Duplicate(String),
    /// Anything else.
    #[error("internal error: {0}")]
    Internal(String),
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
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = self.to_anthropic_response();
        // Note: Error responses do not include `x-request-id`. This is an
        // observability gap -- the request ID is only added in the
        // non-streaming success path. Consider adding it via response
        // middleware or storing the request ID in response extensions.
        (status, body).into_response()
    }
}
