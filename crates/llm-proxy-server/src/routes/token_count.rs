//! `/providers/{provider}/v1/messages/count_tokens` handler.
//!
//! Accepts an Anthropic-format request, decodes it through the Anthropic client
//! adapter into a `CoreRequest`, and estimates the token count from the core
//! representation.

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, Response};
use axum::response::IntoResponse;
use llm_proxy_core::MessageContent;
use llm_proxy_protocol::anthropic::MessageRequest;
use llm_proxy_protocol::client::anthropic;
use serde::Serialize;

use crate::middleware::{OptionalConnectInfo, RequestId};
use crate::state::AppState;

use super::core_pipeline;
use super::error_response::{ClientProtocol, RouteError, route_error_response};

/// Response body for the token count endpoint.
///
/// **Note:** The `input_tokens` value is an approximation based on a heuristic
/// word/character-level counter, not an exact count from the upstream provider's
/// tokenizer. It intentionally excludes tool definitions, non-text content blocks
/// (images, documents), and tool-result content. This estimate is sufficient for
/// gating requests by approximate size but should not be used for precise billing
/// or token accounting.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TokenCountResponse {
    input_tokens: usize,
}

/// POST `/providers/{provider}/v1/messages/count_tokens`
///
/// Accepts an Anthropic-format request, estimates the token count
/// using the heuristic counter, and returns the result.
///
/// This handler uses the core pipeline decode (`anthropic::decode_request`)
/// rather than direct field access on `MessageRequest`, ensuring it works
/// using the configured tokenizer.
pub async fn count_tokens(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Extension(req_id): Extension<RequestId>,
    OptionalConnectInfo(connect_info): OptionalConnectInfo,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match count_tokens_inner(
        &state,
        req_id,
        &provider,
        connect_info.as_ref(),
        &headers,
        body,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error, "request failed");
            route_error_response(ClientProtocol::Anthropic, error)
        }
    }
}

/// Render an axum [`JsonRejection`] as a human-readable message, preserving
/// the distinction between a syntactically invalid JSON body and a body that is
/// valid JSON but does not fit the target type (audit LOW-28).
fn json_rejection_message(rejection: &JsonRejection) -> String {
    use axum::extract::rejection::JsonRejection;
    match rejection {
        JsonRejection::JsonSyntaxError(e) => format!("invalid JSON syntax: {e}"),
        JsonRejection::JsonDataError(e) => format!("JSON body did not match expected type: {e}"),
        JsonRejection::MissingJsonContentType(_) => {
            "missing application/json Content-Type".to_owned()
        }
        other => format!("could not read request body: {other}"),
    }
}

async fn count_tokens_inner(
    state: &AppState,
    req_id: RequestId,
    provider: &str,
    connect_info: Option<&std::net::SocketAddr>,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    // Pre-flight: rate limit, dedup, request ID.
    core_pipeline::validate_provider_name(provider)?;
    if state.providers().get(provider).is_none() {
        return Err(RouteError::UnknownProvider(provider.to_owned()));
    }
    // Reject non-JSON Content-Type before parsing, so a wrong media type is not
    // misreported as a JSON syntax error (audit LOW-30).
    core_pipeline::validate_json_content_type(headers)?;
    let request_path = format!("/providers/{provider}/v1/messages/count_tokens");
    let ctx = core_pipeline::prepare_request(
        state,
        req_id.0,
        headers,
        connect_info,
        &body,
        &request_path,
    )?;
    // Parse and validate the Anthropic MessageRequest.
    //
    // Go through `axum::Json::from_bytes` (rather than `serde_json::from_slice`)
    // so the `JsonRejection` taxonomy is preserved: syntax errors and data
    // (deserialization) errors are surfaced with distinct messages instead of
    // being collapsed into a single opaque "invalid JSON" string.
    let req: MessageRequest = match axum::Json::<MessageRequest>::from_bytes(&body) {
        Ok(axum::Json(value)) => value,
        Err(rejection) => {
            return Err(RouteError::InvalidRequest(json_rejection_message(
                &rejection,
            )));
        }
    };

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
            // Non-text content blocks (images, tool_use, thinking) produce
            // empty text here. This is a known limitation of the heuristic
            // counter. Log at debug level so operators can detect requests
            // where the estimate may be significantly off.
            if text.is_empty() && !msg.content.is_empty() {
                tracing::debug!(
                    role = ?msg.role,
                    blocks = msg.content.len(),
                    "message has content blocks but no extractable text; token estimate will be zero for this message"
                );
            }
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
    fn source_guard_token_count_uses_current_architecture() {
        let source = include_str!("token_count.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);

        assert!(
            !prod.contains("ApiError"),
            "token_count.rs must use ServerError"
        );
        assert!(
            !prod.contains("crate::error::"),
            "token_count.rs must not import from crate::error"
        );
        assert!(
            !prod.contains("content_blocks()"),
            "token_count.rs must not use removed content_blocks() method"
        );
        assert!(
            !prod.contains("system_text()"),
            "token_count.rs must not use removed system_text() method"
        );
    }
}
