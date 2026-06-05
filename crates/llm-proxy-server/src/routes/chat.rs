//! OpenAI Chat Completions route (placeholder).
//!
//! Note: This route is **not mounted** in the router until Phase 9.
//! The types defined here (`ChatRequest`, `ChatMessage`, `ChatResponse`, etc.)
//! are placeholders that shadow the protocol crate types. When Phase 9 mounts
//! this route, these types should be replaced by the protocol crate types
//! (`llm_proxy_protocol::openai::*`) to avoid maintaining parallel type
//! definitions.

#![allow(dead_code)] // Intentionally unmounted until Phase 9.

use axum::extract::State;
use axum_serde::Sonic;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// OpenAI-shaped chat completion request.
#[derive(Deserialize)]
pub(crate) struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
}

/// A single message in the chat request.
#[derive(Deserialize)]
pub(crate) struct ChatMessage {
    role: String,
    content: Value,
}

/// OpenAI-shaped chat completion response.
#[derive(Serialize)]
pub(crate) struct ChatResponse {
    id: String,
    object: &'static str,
    created: i64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

/// A single choice in the response.
#[derive(Serialize)]
pub(crate) struct Choice {
    index: u32,
    message: ResponseMessage,
    finish_reason: &'static str,
}

/// The assistant response message.
#[derive(Serialize)]
pub(crate) struct ResponseMessage {
    role: &'static str,
    content: String,
}

/// Token usage statistics.
#[derive(Serialize)]
pub(crate) struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// POST /v1/chat/completions — non-streaming echo.
///
/// Accepts any well-formed JSON, rejects `stream: true` with 400,
/// otherwise returns the last user message echoed back wrapped in
/// the OpenAI chat completion shape. Uses `Sonic` (sonic-rs, SIMD) on
/// both the request and response — this is the one route where payload
/// size justifies it; ops/error routes stay on serde_json's `Json`.
pub async fn echo_chat(
    State(_state): State<AppState>,
    Sonic(req): Sonic<ChatRequest>,
) -> Result<Sonic<ChatResponse>, ApiError> {
    if req.stream {
        return Err(ApiError::BadRequest(
            "streaming not supported in this build".to_string(),
        ));
    }
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .ok_or_else(|| ApiError::BadRequest("no user message in request".to_string()))?;
    let text = match &last_user.content {
        Value::String(s) => s.clone(),
        other => {
            warn!(?other, "non-string user content coerced to JSON");
            other.to_string()
        }
    };
    Ok(Sonic(ChatResponse {
        id: format!("chatcmpl-{}", Uuid::new_v4()),
        object: "chat.completion",
        created: now_unix(),
        model: req.model,
        choices: vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: "assistant",
                content: text,
            },
            finish_reason: "stop",
        }],
        usage: Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
    }))
}

/// Current Unix time in seconds. The `unwrap_or(0)` is a documented
/// exception to the no-`unwrap` rule (Ch. 4.2): `duration_since`
/// only errors if the wall clock is set before 1970, which we treat
/// as a benign `0` rather than panicking. (Note: `SystemTime` is the
/// wall clock and can move backwards — that is exactly why this
/// returns a `Result` we must handle.)
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
