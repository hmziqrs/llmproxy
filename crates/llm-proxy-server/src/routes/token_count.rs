//! `/v1/messages/count_tokens` handler.
//!
//! Accepts an Anthropic-format request, decodes it through the Anthropic client
//! adapter into a `CoreRequest`, and estimates the token count from the core
//! representation. No legacy state is required.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Response};
use axum::response::IntoResponse;
use llm_proxy_protocol::anthropic::MessageRequest;
use llm_proxy_protocol::client::anthropic;
use llm_proxy_core::MessageContent;
use serde::Serialize;

use crate::state::AppState;

use super::core_pipeline;
use super::error_response::{ClientProtocol, RouteError, route_error_response};

/// Response body for the token count endpoint.
#[derive(Serialize)]
pub(crate) struct TokenCountResponse {
    input_tokens: usize,
    /// Non-standard extension: `token_count` duplicates `input_tokens`.
    /// The Anthropic Messages API `count_tokens` endpoint returns only
    /// `input_tokens`. This extra field is retained for backward
    /// compatibility with existing clients.
    token_count: usize,
}

/// POST `/v1/messages/count_tokens`
///
/// Accepts an Anthropic-format request, estimates the token count
/// using the heuristic counter, and returns the result.
///
/// This handler uses the core pipeline decode (`anthropic::decode_request`)
/// rather than direct field access on `MessageRequest`, ensuring it works
/// without legacy state.
pub async fn count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match count_tokens_inner(state, headers, body).await {
        Ok(response) => response,
        Err(error) => route_error_response(ClientProtocol::Anthropic, error),
    }
}

async fn count_tokens_inner(
    state: AppState,
    _headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    // Parse and validate the Anthropic MessageRequest.
    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;

    req.validate()
        .map_err(|e| RouteError::InvalidRequest(e))?;

    // Decode through the Anthropic client adapter to get a CoreRequest.
    // This validates the request shape and normalises it.
    let core = anthropic::decode_request(req)
        .map_err(core_pipeline::protocol_error_to_route)?;

    // Extract text content from core messages for token counting.
    let system_text: String = core
        .system
        .iter()
        .filter_map(|block| {
            match block {
                llm_proxy_protocol::core::CoreContent::Text { text, .. } => Some(text.as_str()),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join("");

    let messages: Vec<MessageContent> = core
        .messages
        .iter()
        .map(|msg| {
            let text: String = msg
                .content
                .iter()
                .filter_map(|block| {
                    match block {
                        llm_proxy_protocol::core::CoreContent::Text { text, .. } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    }
                })
                .collect();
            MessageContent::new(
                match msg.role {
                    llm_proxy_protocol::core::CoreRole::User => "user",
                    llm_proxy_protocol::core::CoreRole::Assistant => "assistant",
                    _ => "user",
                },
                text,
            )
        })
        .collect();

    let count = state.token_counter.count_messages(&system_text, &messages);

    let response = axum::Json(TokenCountResponse {
        input_tokens: count,
        token_count: count,
    });

    Ok(response.into_response())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    #[test]
    fn source_guard_token_count_no_legacy_imports() {
        let source = include_str!("token_count.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);

        assert!(
            !prod.contains("ApiError"),
            "token_count.rs must not use legacy ApiError"
        );
        assert!(
            !prod.contains("crate::error::"),
            "token_count.rs must not import from crate::error"
        );
        assert!(
            !prod.contains("content_blocks()"),
            "token_count.rs must not use legacy content_blocks() method"
        );
        assert!(
            !prod.contains("system_text()"),
            "token_count.rs must not use legacy system_text() method"
        );
    }
}
