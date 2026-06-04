use axum::{Json, extract::State};
use llm_proxy_core::MessageContent;
use llm_proxy_protocol::anthropic::MessageRequest;
use serde::Serialize;

use crate::error::ApiError;
use crate::state::AppState;

/// Response body for the token count endpoint.
#[derive(Serialize)]
pub(crate) struct TokenCountResponse {
    input_tokens: usize,
    token_count: usize,
}

/// POST /v1/messages/count_tokens
///
/// Accepts an Anthropic-format request, estimates the token count
/// using the heuristic counter, and returns the result.
pub async fn count_tokens(
    State(state): State<AppState>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<TokenCountResponse>, ApiError> {
    // Validate the request.
    req.validate().map_err(ApiError::BadRequest)?;

    // Extract system text and messages for counting.
    let system_text = req.system_text();

    let messages: Vec<MessageContent> = req
        .messages
        .iter()
        .map(|msg| {
            let blocks = msg.content_blocks();
            let text: String = blocks
                .iter()
                .filter_map(|b| {
                    if b.r#type == "text" {
                        b.text.as_deref()
                    } else {
                        None
                    }
                })
                .collect();
            MessageContent::new(&msg.role, text)
        })
        .collect();

    let count = state.token_counter.count_messages(&system_text, &messages);

    Ok(Json(TokenCountResponse {
        input_tokens: count,
        token_count: count,
    }))
}
