//! `/v1/chat/completions` handler using the core pipeline.
//!
//! This is the Phase 9 implementation: the handler parses the incoming OpenAI
//! `ChatCompletionRequest`, decodes it into a `CoreRequest` via the OpenAI Chat
//! client adapter, and then dispatches through the shared core pipeline
//! ([`handle_core_once`] or [`handle_core_stream`]).
//!
//! **No legacy imports**: no scenario detection, endpoint classification,
//! fallback chains, legacy HTTP client, or provider-specific stream handlers.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Response};
use llm_proxy_protocol::client::openai_chat;
use llm_proxy_protocol::openai::ChatCompletionRequest;
use tracing::info;

use crate::state::AppState;

use super::core_pipeline;
use super::error_response::{ClientProtocol, RouteError, route_error_response};

/// POST `/v1/chat/completions`
///
/// Accepts an OpenAI-format [`ChatCompletionRequest`], decodes it through the
/// core pipeline, and returns an OpenAI-shaped response.
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match handle_chat_completions_inner(state, headers, body).await {
        Ok(response) => response,
        Err(error) => {
            info!(error = %error, "request failed");
            route_error_response(ClientProtocol::OpenAiChat, error)
        }
    }
}

/// Inner handler that returns `Result` so errors can be mapped uniformly.
async fn handle_chat_completions_inner(
    state: AppState,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    // Pre-flight: rate limit, dedup, request ID.
    // Note: prepare_request takes a &[u8] reference (non-consuming) so the
    // body bytes remain available for the second parse below. The double
    // deserialization is intentional: prepare_request needs raw bytes for
    // dedup hashing before we know the request type.
    let ctx = core_pipeline::prepare_request(&state, &headers, &body)?;

    // Parse the OpenAI ChatCompletionRequest.
    let req: ChatCompletionRequest = serde_json::from_slice(&body)
        .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;

    // Decode the OpenAI Chat request into a core request.
    // Note: ChatCompletionRequest does not have a separate validate() method
    // (unlike the Anthropic handler). All validation is performed inside
    // decode_request: it checks for non-empty model and non-empty messages.
    let core = openai_chat::decode_request(req)
        .map_err(core_pipeline::protocol_error_to_route)?;

    let is_streaming = core.stream;
    info!(
        request_id = %ctx.request_id,
        model = %core.model.requested,
        streaming = is_streaming,
        "decoded OpenAI Chat request into CoreRequest"
    );

    // Dispatch to streaming or non-streaming pipeline.
    if is_streaming {
        core_pipeline::handle_core_stream(state, ctx, core, ClientProtocol::OpenAiChat).await
    } else {
        core_pipeline::handle_core_once(state, ctx, core, ClientProtocol::OpenAiChat).await
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    // -- Source guard: no legacy imports ---------------------------------------

    #[test]
    fn source_guard_no_legacy_imports() {
        let source = include_str!("chat.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);

        assert!(
            !prod.contains("llm_proxy_core::router"),
            "chat.rs must not import legacy router (scenario/fallback)"
        );
        assert!(
            !prod.contains("detect_scenario"),
            "chat.rs must not use detect_scenario"
        );
        assert!(
            !prod.contains("route_for_streaming"),
            "chat.rs must not use route_for_streaming"
        );
        assert!(
            !prod.contains("classify_endpoint"),
            "chat.rs must not use classify_endpoint"
        );
        assert!(
            !prod.contains("EndpointType"),
            "chat.rs must not use EndpointType"
        );
        assert!(
            !prod.contains("OpenCodeClient"),
            "chat.rs production code must not use OpenCodeClient"
        );
        assert!(
            !prod.contains("transformer"),
            "chat.rs must not use legacy transformer module"
        );
        assert!(
            !prod.contains("StreamProxy"),
            "chat.rs must not use StreamProxy"
        );
        assert!(
            !prod.contains("spawn_proxy_task"),
            "chat.rs must not use spawn_proxy_task"
        );
        assert!(
            !prod.contains("handle_anthropic_streaming"),
            "chat.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("handle_openai_streaming"),
            "chat.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("handle_responses_streaming"),
            "chat.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("handle_gemini_streaming"),
            "chat.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("ApiError"),
            "chat.rs must not use legacy ApiError"
        );
        assert!(
            !prod.contains("ScenarioConfig"),
            "chat.rs must not use ScenarioConfig"
        );
        assert!(
            !prod.contains("axum_serde"),
            "chat.rs must not use axum_serde (legacy echo handler)"
        );
        assert!(
            !prod.contains("Sonic"),
            "chat.rs must not use Sonic (legacy echo handler)"
        );
    }
}
