//! `/v1/messages/count_tokens` handler.
//!
//! Accepts an Anthropic-format request, decodes it through the Anthropic client
//! adapter into a `CoreRequest`, and estimates the token count from the core
//! representation. No legacy state is required.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Response};
use axum::response::IntoResponse;
use llm_proxy_core::MessageContent;
use llm_proxy_protocol::anthropic::MessageRequest;
use llm_proxy_protocol::client::anthropic;
use serde::Serialize;

use crate::state::AppState;

use super::core_pipeline;
use super::error_response::{ClientProtocol, RouteError, route_error_response};

/// Response body for the token count endpoint.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenCountResponse {
    input_tokens: usize,
    /// Non-standard extension: `token_count` duplicates `input_tokens`.
    /// The Anthropic Messages API `count_tokens` endpoint returns only
    /// `input_tokens`. This extra field is retained for backward
    /// compatibility with existing clients that depend on this field name.
    ///
    /// TODO(phase-12): Remove this field once all known clients are migrated
    /// to use `input_tokens` only. Track migration status before removing.
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
    match count_tokens_inner(&state, &headers, body).await {
        Ok(response) => response,
        Err(error) => route_error_response(ClientProtocol::Anthropic, error),
    }
}

async fn count_tokens_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    // Pre-flight: rate limit, dedup, request ID.
    let ctx = core_pipeline::prepare_request(state, headers, &body)?;
    // Parse and validate the Anthropic MessageRequest.
    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;

    req.validate()
        .map_err(|e| RouteError::InvalidRequest(e.to_string()))?;

    // Decode through the Anthropic client adapter to get a CoreRequest.
    // This validates the request shape and normalises it.
    let core = anthropic::decode_request(req).map_err(core_pipeline::protocol_error_to_route)?;

    // Extract text content from core messages for token counting.
    //
    // NOTE: This estimate intentionally only counts text from `CoreContent::Text`
    // blocks. The following are NOT counted:
    //   - Tool definitions (`core.tools`) -- their name, description, and
    //     input_schema text fields are excluded.
    //   - Non-text content blocks (images, documents, etc.).
    //   - Tool result content blocks.
    //   - System prompt non-text blocks.
    //
    // TODO(future): Extend counting to include tool definitions, tool_use blocks,
    // and tool_result blocks for a more accurate estimate.
    // Collect system text without intermediate Vec allocation.
    let system_text: String = core
        .system
        .iter()
        .filter_map(|block| match block {
            llm_proxy_protocol::core::CoreContent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .fold(String::new(), |mut acc, s| {
            acc.push_str(s);
            acc
        });

    let messages: Vec<MessageContent> = core
        .messages
        .iter()
        .map(|msg| {
            let text: String = msg
                .content
                .iter()
                .filter_map(|block| match block {
                    llm_proxy_protocol::core::CoreContent::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let role_name = match msg.role {
                llm_proxy_protocol::core::CoreRole::User => "user",
                llm_proxy_protocol::core::CoreRole::Assistant => "assistant",
                llm_proxy_protocol::core::CoreRole::System => "system",
                // The Anthropic Messages API does not have a 'tool' role.
                // Tool results are carried in 'user' role messages with
                // tool_result content blocks. If a CoreRole::Tool reaches
                // here, treat it as 'user' since tool_result content is
                // typically nested under the user role in Anthropic.
                llm_proxy_protocol::core::CoreRole::Tool => {
                    tracing::debug!(
                        "CoreRole::Tool mapped to 'user' for token counting; \
                         tool_result content is counted under user role"
                    );
                    "user"
                }
                // All known variants are handled above. This catch-all exists
                // for forward compatibility. Unknown roles are treated as
                // 'user' since that is the least-lossy mapping.
                _ => {
                    tracing::warn!(
                        role = ?msg.role,
                        "unknown CoreRole variant in token count, mapping to 'user'"
                    );
                    "user"
                }
            };
            MessageContent::new(role_name, text)
        })
        .collect();

    let count = state.token_counter.count_messages(&system_text, &messages);

    let response = axum::Json(TokenCountResponse {
        input_tokens: count,
        token_count: count,
    });

    let mut response = response.into_response();

    // Defense-in-depth: explicitly set Content-Type even though
    // axum::Json's IntoResponse already sets it internally.
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    // Insert the request ID header safely.
    response.headers_mut().insert(
        "x-request-id",
        ctx.request_id
            .parse()
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("unknown")),
    );

    Ok(response)
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
