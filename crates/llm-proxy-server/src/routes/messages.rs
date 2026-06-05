//! `/v1/messages` handler using the core pipeline.
//!
//! This is the Phase 8 rewrite: the handler parses the incoming Anthropic
//! `MessageRequest`, decodes it into a `CoreRequest` via the Anthropic client
//! adapter, and then dispatches through the shared core pipeline
//! ([`handle_core_once`] or [`handle_core_stream`]).
//!
//! **No legacy imports**: no scenario detection, endpoint classification,
//! fallback chains, legacy HTTP client, or provider-specific stream handlers.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Response};
use llm_proxy_protocol::anthropic::MessageRequest;
use llm_proxy_protocol::client::anthropic;
use tracing::info;

use crate::state::AppState;

use super::core_pipeline;
use super::error_response::{ClientProtocol, RouteError, route_error_response};

/// POST `/v1/messages`
///
/// Accepts an Anthropic-format [`MessageRequest`], decodes it through the
/// core pipeline, and returns an Anthropic-shaped response.
pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match handle_messages_inner(state, headers, body).await {
        Ok(response) => response,
        Err(error) => {
            info!(error = %error, "request failed");
            route_error_response(ClientProtocol::Anthropic, error)
        }
    }
}

/// Inner handler that returns `Result` so errors can be mapped uniformly.
async fn handle_messages_inner(
    state: AppState,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    // Pre-flight: rate limit, dedup, request ID.
    let ctx = core_pipeline::prepare_request(&state, &headers, &body)?;

    // Parse and validate the Anthropic MessageRequest.
    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;

    req.validate()
        .map_err(|e| RouteError::InvalidRequest(e))?;

    // Decode the Anthropic request into a core request.
    let core = anthropic::decode_request(req)
        .map_err(core_pipeline::protocol_error_to_route)?;

    let is_streaming = core.stream;
    info!(
        request_id = %ctx.request_id,
        model = %core.model.requested,
        streaming = is_streaming,
        "decoded Anthropic request into CoreRequest"
    );

    // Dispatch to streaming or non-streaming pipeline.
    if is_streaming {
        core_pipeline::handle_core_stream(state, ctx, core, ClientProtocol::Anthropic).await
    } else {
        core_pipeline::handle_core_once(state, ctx, core, ClientProtocol::Anthropic).await
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
        let source = include_str!("messages.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);

        assert!(
            !prod.contains("llm_proxy_core::router"),
            "messages.rs must not import legacy router (scenario/fallback)"
        );
        assert!(
            !prod.contains("detect_scenario"),
            "messages.rs must not use detect_scenario"
        );
        assert!(
            !prod.contains("route_for_streaming"),
            "messages.rs must not use route_for_streaming"
        );
        assert!(
            !prod.contains("classify_endpoint"),
            "messages.rs must not use classify_endpoint"
        );
        assert!(
            !prod.contains("EndpointType"),
            "messages.rs must not use EndpointType"
        );
        assert!(
            !prod.contains("OpenCodeClient"),
            "messages.rs production code must not use OpenCodeClient"
        );
        assert!(
            !prod.contains("transformer"),
            "messages.rs must not use legacy transformer module"
        );
        assert!(
            !prod.contains("StreamProxy"),
            "messages.rs must not use StreamProxy"
        );
        assert!(
            !prod.contains("spawn_proxy_task"),
            "messages.rs must not use spawn_proxy_task"
        );
        assert!(
            !prod.contains("handle_anthropic_streaming"),
            "messages.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("handle_openai_streaming"),
            "messages.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("handle_responses_streaming"),
            "messages.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("handle_gemini_streaming"),
            "messages.rs must not use provider-specific stream handlers"
        );
        assert!(
            !prod.contains("ApiError"),
            "messages.rs must not use legacy ApiError"
        );
        assert!(
            !prod.contains("ScenarioConfig"),
            "messages.rs must not use ScenarioConfig"
        );
    }
}
