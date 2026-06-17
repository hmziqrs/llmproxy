//! `/providers/{provider}/v1/chat/completions` handler using the core pipeline.
//!
//! The handler parses the incoming OpenAI `ChatCompletionRequest`, decodes it
//! into a `CoreRequest` via the OpenAI Chat
//! client adapter, and then dispatches through the shared core pipeline
//! ([`handle_core_once`] or [`handle_core_stream`]).
//!
//! Architecture boundary: route handlers do not perform scenario detection,
//! endpoint classification, fallback routing, or provider-specific streaming.

use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, Response, header};
use axum::Json;
use llm_proxy_core::ProviderRouteKind;
use llm_proxy_protocol::client::openai_chat;
use llm_proxy_protocol::openai::ChatCompletionRequest;
use tracing::{info, warn};

use crate::middleware::{OptionalConnectInfo, RequestId};
use crate::state::AppState;

use super::core_pipeline;
use super::error_response::{ClientProtocol, RouteError, route_error_response};

/// POST `/providers/{provider}/v1/chat/completions`
///
/// Accepts an OpenAI-format [`ChatCompletionRequest`], decodes it through the
/// core pipeline, and returns an OpenAI-shaped response.
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Extension(req_id): Extension<RequestId>,
    OptionalConnectInfo(connect_info): OptionalConnectInfo,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match handle_chat_completions_inner(state, req_id, provider, connect_info, headers, body).await {
        Ok(response) => response,
        Err(error) => {
            warn!(error = %error, "request failed");
            route_error_response(ClientProtocol::OpenAiChat, error)
        }
    }
}

/// Inner handler that returns `Result` so errors can be mapped uniformly.
async fn handle_chat_completions_inner(
    state: AppState,
    req_id: RequestId,
    provider: String,
    connect_info: Option<std::net::SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    // Input validation gates, ordered cheapest-first to avoid charging
    // rate-limit/dedup budget for malformed requests. Mirrors the ordering
    // established in `token_count.rs`: provider name -> existence ->
    // content-type -> rate-limit/dedup -> JSON parse.
    core_pipeline::validate_provider_name(&provider)?;
    if state.providers().get(&provider).is_none() {
        return Err(RouteError::UnknownProvider(provider.clone()));
    }
    validate_json_content_type(&headers)?;

    let request_path = format!("/providers/{provider}/v1/chat/completions");
    let ctx = core_pipeline::prepare_request(
        &state,
        req_id.0,
        &headers,
        connect_info.as_ref(),
        &body,
        &request_path,
    )?;

    // Parse the OpenAI ChatCompletionRequest via axum's `Json` helper so the
    // `JsonRejection` taxonomy (syntax vs data error) is preserved and mapped
    // to a precise 400 body, rather than collapsing both into one opaque
    // "invalid JSON" string.
    let req: ChatCompletionRequest = match Json::<ChatCompletionRequest>::from_bytes(&body) {
        Ok(Json(value)) => value,
        Err(rejection) => return Err(json_rejection_to_route_error(rejection)),
    };

    // Decode the OpenAI Chat request into a core request.
    // Note: ChatCompletionRequest does not have a separate validate() method
    // (unlike the Anthropic handler). All validation is performed inside
    // decode_request: it checks for non-empty model and non-empty messages.
    let core = openai_chat::decode_request(req).map_err(core_pipeline::protocol_error_to_route)?;

    let is_streaming = core.stream;
    info!(
        request_id = %ctx.request_id,
        provider = %provider,
        model = %core.model.requested,
        streaming = is_streaming,
        "decoded OpenAI Chat request into CoreRequest"
    );

    // Dispatch to streaming or non-streaming pipeline.
    if is_streaming {
        core_pipeline::handle_core_stream(
            state,
            ctx,
            &provider,
            ProviderRouteKind::ChatCompletions,
            core,
            ClientProtocol::OpenAiChat,
        )
        .await
    } else {
        core_pipeline::handle_core_once(
            state,
            ctx,
            &provider,
            ProviderRouteKind::ChatCompletions,
            core,
            ClientProtocol::OpenAiChat,
        )
        .await
    }
}

/// Reject requests whose `Content-Type` is present but not a JSON media type.
///
/// Handlers accept `axum::body::Bytes` (so the raw body can be hashed for
/// dedup), which bypasses the `Json` extractor's built-in content-type check.
/// This restores the spirit of that check: a body that is not advertised as
/// JSON should not be reported as a misleading "invalid JSON" parse error.
/// A missing `Content-Type` is tolerated for backwards compatibility, matching
/// the prior behaviour.
fn validate_json_content_type(headers: &HeaderMap) -> Result<(), RouteError> {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return Ok(());
    };
    let Ok(ct) = value.to_str() else {
        return Ok(());
    };
    // Accept `application/json` and any `+json` suffix (e.g.
    // `application/vnd.api+json`). Strip any `; charset=...` parameters first.
    let essence = ct.split(';').next().unwrap_or(ct).trim().to_ascii_lowercase();
    let is_json = essence == "application/json" || essence.ends_with("+json");
    if !is_json {
        return Err(RouteError::InvalidRequest(format!(
            "expected application/json Content-Type, got {ct}"
        )));
    }
    Ok(())
}

/// Map an axum `JsonRejection` to a `RouteError`, preserving the distinction
/// between a syntactically invalid JSON body and a body that is valid JSON but
/// does not fit the target type.
fn json_rejection_to_route_error(rejection: axum::extract::rejection::JsonRejection) -> RouteError {
    use axum::extract::rejection::JsonRejection;
    match rejection {
        JsonRejection::JsonSyntaxError(e) => {
            RouteError::InvalidRequest(format!("invalid JSON syntax: {e}"))
        }
        JsonRejection::JsonDataError(e) => {
            RouteError::InvalidRequest(format!("JSON body did not match expected type: {e}"))
        }
        JsonRejection::MissingJsonContentType(_) => {
            RouteError::InvalidRequest("missing application/json Content-Type".to_owned())
        }
        // BytesRejection and any future non_exhaustive variant: surface a
        // generic but informative invalid-request body.
        other => RouteError::InvalidRequest(format!("could not read request body: {other}")),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    // -- Architecture source guard ---------------------------------------------

    #[test]
    fn source_guard_enforces_route_boundaries() {
        let source = include_str!("chat.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);

        assert!(
            !prod.contains("llm_proxy_core::router"),
            "chat.rs must not import scenario/fallback routing"
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
            "chat.rs must not use the removed transformer module"
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
        assert!(!prod.contains("ApiError"), "chat.rs must use RouteError");
        assert!(
            !prod.contains("ScenarioConfig"),
            "chat.rs must not use ScenarioConfig"
        );
        assert!(
            !prod.contains("axum_serde"),
            "chat.rs must not use the removed echo handler"
        );
        assert!(
            !prod.contains("Sonic"),
            "chat.rs must not use the removed echo handler"
        );
    }
}
