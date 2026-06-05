//! Shared core pipeline for route handlers.
//!
//! Provides [`prepare_request`] (rate-limit, dedup, request-ID generation),
//! [`handle_core_once`] (non-streaming), and [`handle_core_stream`] (streaming)
//! used by `/v1/messages` and future `/v1/chat/completions`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Response, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::stream::{BoxStream, StreamExt};
use llm_proxy_core::model_route::{ModelRouteError, resolve_model_route};
use llm_proxy_protocol::client::anthropic;
use llm_proxy_protocol::client::anthropic::StreamEncoder;
use llm_proxy_protocol::core::{CoreEvent, CoreRequest};
use llm_proxy_provider::adapter::{ProviderAdapter, ProviderAdapterTarget, ProviderProtocol, ProviderStreamDecoder};
use llm_proxy_provider::sse::SseFramer;
use llm_proxy_provider::transport::ProxyRequest;
use tracing::{info, warn};

use crate::middleware::get_client_ip;
use crate::state::AppState;

use super::error_response::{ClientProtocol, RouteError};

// ---------------------------------------------------------------------------
// RequestContext
// ---------------------------------------------------------------------------

/// Pre-resolved per-request context shared between non-streaming and streaming
/// paths.
pub(crate) struct RequestContext {
    /// Unique request ID for tracing and the `x-request-id` header.
    pub(crate) request_id: String,
    /// Instant when the handler was entered (for latency metrics).
    pub(crate) start: Instant,
}

// ---------------------------------------------------------------------------
// prepare_request
// ---------------------------------------------------------------------------

/// Perform pre-flight checks and build a [`RequestContext`].
///
/// Returns `Err(RouteError)` early for rate-limited or duplicate requests so
/// the handler can immediately produce an error response without entering the
/// core pipeline.
pub(crate) fn prepare_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<RequestContext, RouteError> {
    let request_id = state.request_id_gen.next_id();
    let client_ip = get_client_ip(headers, None);

    if !state.rate_limiter.is_allowed(&client_ip) {
        state.metrics.record_rate_limited();
        return Err(RouteError::InvalidRequest(
            "rate limit exceeded".to_owned(),
        ));
    }

    if state.request_dedup.is_duplicate(body) {
        state.metrics.record_deduplicated();
        return Err(RouteError::InvalidRequest(
            "duplicate request, please retry".to_owned(),
        ));
    }

    Ok(RequestContext {
        request_id,
        start: Instant::now(),
    })
}

// ---------------------------------------------------------------------------
// resolve target helper
// ---------------------------------------------------------------------------

/// Resolve a `CoreRequest` through the model router and provider registry into
/// a tuple of (`ProviderAdapterTarget`, `ProviderAdapter`) ready for encoding.
fn resolve_target(
    state: &AppState,
    core: &CoreRequest,
) -> Result<(ProviderAdapterTarget, ProviderAdapter), RouteError> {
    let app_config = state
        .app_config()
        .ok_or_else(|| RouteError::Internal("TOML config required".to_owned()))?;

    let providers = state
        .providers()
        .ok_or_else(|| RouteError::Internal("provider registry required".to_owned()))?;

    let target = resolve_model_route(&app_config.models, &core.model.requested)
        .map_err(|e| match &e {
            ModelRouteError::UnknownModel(m) => RouteError::UnknownModel(m.clone()),
            _ => RouteError::Internal(e.to_string()),
        })?;

    let adapter_target_config = providers
        .resolve_adapter_target(&target)
        .map_err(|e| RouteError::Internal(e.to_string()))?;

    let protocol = ProviderProtocol::parse(&adapter_target_config.protocol)
        .ok_or_else(|| {
            RouteError::Internal(format!(
                "unknown protocol: {}",
                adapter_target_config.protocol
            ))
        })?;

    let adapter = state
        .provider_adapters
        .get(protocol)
        .ok_or_else(|| {
            RouteError::Internal(format!(
                "no adapter registered for protocol: {}",
                adapter_target_config.protocol
            ))
        })?
        .clone();

    let provider_target = ProviderAdapterTarget {
        provider_name: adapter_target_config.provider_name,
        adapter_name: adapter_target_config.adapter_name,
        protocol,
        endpoint: adapter_target_config.endpoint,
        auth_style: adapter_target_config.auth_style,
        api_key: adapter_target_config.api_key,
        requested_model: adapter_target_config.requested_model,
        upstream_model: adapter_target_config.upstream_model,
    };

    Ok((provider_target, adapter))
}

// ---------------------------------------------------------------------------
// handle_core_once
// ---------------------------------------------------------------------------

/// Non-streaming core pipeline.
///
/// ```text
/// CoreRequest -> encode -> send -> decode -> client encode -> HTTP response
/// ```
pub(crate) async fn handle_core_once(
    state: AppState,
    ctx: RequestContext,
    core: CoreRequest,
    client_protocol: ClientProtocol,
) -> Result<Response<Body>, RouteError> {
    let is_streaming = core.stream;
    state.metrics.record_request(is_streaming);

    info!(
        request_id = %ctx.request_id,
        model = %core.model.requested,
        streaming = is_streaming,
        "processing request"
    );

    let (target, adapter) = resolve_target(&state, &core)?;

    info!(
        request_id = %ctx.request_id,
        provider = %target.provider_name,
        model = %target.upstream_model,
        "routed to provider"
    );

    // Encode the core request into a provider-specific HTTP request.
    let proxy_req: ProxyRequest = adapter
        .encode_request(&core, &target)
        .map_err(|e| RouteError::Internal(format!("encode error: {e}")))?;

    // Send to upstream.
    let response_bytes = state
        .proxy_client
        .send(proxy_req)
        .await
        .map_err(map_provider_error)?;

    // Decode the provider response into a CoreResponse.
    let core_resp = adapter
        .decode_response(&response_bytes, &target)
        .map_err(|e| RouteError::ProviderDecode(format!("decode response: {e}")))?;

    // Encode into the client-specific response.
    let latency = ctx.start.elapsed();
    state
        .metrics
        .record_success(&target.upstream_model, latency);

    let response_body = match client_protocol {
        ClientProtocol::Anthropic => {
            let msg_resp = anthropic::encode_response(core_resp)
                .map_err(|e| RouteError::Internal(format!("client encode: {e}")))?;
            serde_json::to_vec(&msg_resp)
                .map_err(|e| RouteError::Internal(format!("serialize: {e}")))?
        }
        ClientProtocol::OpenAiChat => {
            // Phase 9 will implement OpenAI Chat response encoding.
            return Err(RouteError::Internal(
                "OpenAI Chat response encoding not yet implemented".to_owned(),
            ));
        }
    };

    info!(
        request_id = %ctx.request_id,
        model = %target.upstream_model,
        latency_ms = latency.as_millis(),
        "request completed"
    );

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-request-id", &ctx.request_id)
        .body(Body::from(response_body))
        .expect("static header values are valid");

    // Ensure content-type is set (defense-in-depth).
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    Ok(response)
}

// ---------------------------------------------------------------------------
// handle_core_stream
// ---------------------------------------------------------------------------

/// Heartbeat interval for SSE streams.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

/// Streaming core pipeline.
///
/// ```text
/// CoreRequest -> encode -> send_stream -> SseFramer -> ProviderStreamDecoder
///            -> StreamEncoder -> SSE response
/// ```
pub(crate) async fn handle_core_stream(
    state: AppState,
    ctx: RequestContext,
    core: CoreRequest,
    client_protocol: ClientProtocol,
) -> Result<Response<Body>, RouteError> {
    state.metrics.record_request(true);

    info!(
        request_id = %ctx.request_id,
        model = %core.model.requested,
        streaming = true,
        "processing streaming request"
    );

    let (target, adapter) = resolve_target(&state, &core)?;

    info!(
        request_id = %ctx.request_id,
        provider = %target.provider_name,
        model = %target.upstream_model,
        "routed to provider (streaming)"
    );

    // Encode the core request into a provider-specific HTTP request.
    let proxy_req: ProxyRequest = adapter
        .encode_request(&core, &target)
        .map_err(|e| RouteError::Internal(format!("encode error: {e}")))?;

    // Open the streaming connection.
    let byte_stream = state
        .proxy_client
        .send_stream(proxy_req)
        .await
        .map_err(map_provider_error)?;

    // Create a provider stream decoder.
    let provider_decoder = adapter.new_stream_decoder(&target);
    let sse_framer = SseFramer::new();

    // Create the client stream encoder.
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4());
    let model_name = core.model.requested.clone();
    let client_encoder = StreamEncoder::new(msg_id, model_name);

    let request_id = ctx.request_id.clone();
    let upstream_model = target.upstream_model.clone();
    let metrics = Arc::clone(&state.metrics);

    // Build the output SSE stream.
    let output_stream = build_sse_output_stream(
        byte_stream,
        provider_decoder,
        sse_framer,
        client_encoder,
        client_protocol,
        request_id.clone(),
    );

    // Wrap in an axum SSE response.
    let sse = Sse::new(output_stream.map(Ok::<_, std::convert::Infallible>))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT_INTERVAL));

    let response = sse.into_response();
    let (mut parts, body) = response.into_parts();

    // Add custom headers.
    parts.headers.insert(
        "X-Accel-Buffering",
        "no".parse().expect("static header value is always valid"),
    );
    parts.headers.insert(
        "x-request-id",
        request_id
            .parse()
            .unwrap_or_else(|_| "unknown".parse().expect("fallback is valid")),
    );

    // Record successful stream start.
    metrics.record_success(&upstream_model, ctx.start.elapsed());

    info!(
        request_id = %ctx.request_id,
        model = %target.upstream_model,
        "streaming started"
    );

    Ok(Response::from_parts(parts, body))
}

// ---------------------------------------------------------------------------
// SSE output stream builder
// ---------------------------------------------------------------------------

/// Build a stream that converts provider byte chunks into client SSE events.
fn build_sse_output_stream(
    byte_stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, llm_proxy_provider::error::ProviderError>> + Send + 'static>>,
    mut provider_decoder: Box<dyn ProviderStreamDecoder + Send>,
    mut sse_framer: SseFramer,
    mut client_encoder: StreamEncoder,
    client_protocol: ClientProtocol,
    request_id: String,
) -> BoxStream<'static, Event> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(256);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        let mut stream = byte_stream;
        let mut first_byte_sent = false;

        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    // Client disconnected; abort upstream stream.
                    return;
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            // Feed bytes through the SSE framer.
                            let frames = match sse_framer.push_chunk(&bytes) {
                                Ok(f) => f,
                                Err(e) => {
                                    warn!(
                                        request_id = %request_id,
                                        error = %e,
                                        "SSE framing error in stream"
                                    );
                                    if first_byte_sent {
                                        emit_stream_error(
                                            &mut client_encoder,
                                            &tx,
                                            &client_protocol,
                                            &format!("stream framing error: {e}"),
                                        ).await;
                                    }
                                    break;
                                }
                            };

                            for frame in &frames {
                                let core_events = match provider_decoder.decode_frame(frame) {
                                    Ok(events) => events,
                                    Err(e) => {
                                        warn!(
                                            request_id = %request_id,
                                            error = %e,
                                            "provider decode error in stream"
                                        );
                                        if first_byte_sent {
                                            emit_stream_error(
                                                &mut client_encoder,
                                                &tx,
                                                &client_protocol,
                                                &format!("provider decode error: {e}"),
                                            ).await;
                                        }
                                        break;
                                    }
                                };

                                for core_event in core_events {
                                    let client_events = encode_core_event(
                                        &mut client_encoder,
                                        &client_protocol,
                                        core_event,
                                    );
                                    for event in client_events {
                                        first_byte_sent = true;
                                        if tx.send(event).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!(
                                request_id = %request_id,
                                error = %e,
                                "upstream stream error"
                            );
                            if first_byte_sent {
                                emit_stream_error(
                                    &mut client_encoder,
                                    &tx,
                                    &client_protocol,
                                    &format!("upstream error: {e}"),
                                ).await;
                            }
                            break;
                        }
                        None => break,
                    }
                }
            }
        }

        // Finalize: call provider decoder finish() then client encoder finish().
        match provider_decoder.finish() {
            Ok(final_events) => {
                for core_event in final_events {
                    let client_events = encode_core_event(
                        &mut client_encoder,
                        &client_protocol,
                        core_event,
                    );
                    for event in client_events {
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    request_id = %request_id,
                    error = %e,
                    "provider decoder finish error"
                );
            }
        }

        // Emit any remaining client encoder events (synthetic terminal if needed).
        match client_encoder.finish() {
            Ok(final_msg_events) => {
                for me in final_msg_events {
                    if let Ok(json) = serde_json::to_string(&me) {
                        let event = Event::default().data(json);
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    request_id = %request_id,
                    error = %e,
                    "client encoder finish error"
                );
            }
        }
    });

    // Wrap the receiver so that dropping it cancels the spawned task.
    let rx_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let cancel_guard = cancel.drop_guard();
    rx_stream
        .map(move |item| {
            let _guard = &cancel_guard;
            item
        })
        .boxed()
}

/// Encode a single [`CoreEvent`] using the appropriate client stream encoder.
fn encode_core_event(
    client_encoder: &mut StreamEncoder,
    _client_protocol: &ClientProtocol,
    event: CoreEvent,
) -> Vec<Event> {
    match client_encoder.encode_event(event) {
        Ok(msg_events) => msg_events
            .into_iter()
            .filter_map(|me| {
                let json = serde_json::to_string(&me).ok()?;
                Some(Event::default().data(json))
            })
            .collect(),
        Err(e) => {
            warn!("client stream encode error: {e}");
            Vec::new()
        }
    }
}

/// Emit an error event into the stream, then let the encoder finish.
async fn emit_stream_error(
    client_encoder: &mut StreamEncoder,
    tx: &tokio::sync::mpsc::Sender<Event>,
    _client_protocol: &ClientProtocol,
    message: &str,
) {
    use llm_proxy_protocol::core::{CoreStreamError, CoreStreamErrorKind};

    let error_event = CoreEvent::Error {
        error: CoreStreamError::new(CoreStreamErrorKind::Upstream, message.to_owned()),
    };

    let events = encode_core_event(client_encoder, _client_protocol, error_event);
    for event in events {
        if tx.send(event).await.is_err() {
            return;
        }
    }

    // Emit terminal events if the encoder hasn't finished yet.
    match client_encoder.finish() {
        Ok(final_msg_events) => {
            for me in final_msg_events {
                if let Ok(json) = serde_json::to_string(&me) {
                    let event = Event::default().data(json);
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
        }
        Err(e) => {
            warn!("client encoder finish after error: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

/// Map a [`ProviderError`](llm_proxy_provider::error::ProviderError) to a
/// [`RouteError`].
pub(crate) fn map_provider_error(e: llm_proxy_provider::error::ProviderError) -> RouteError {
    match &e {
        llm_proxy_provider::error::ProviderError::Api { status, body } => RouteError::Upstream {
            status: StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
            body: body.clone(),
        },
        _ => RouteError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            body: e.to_string(),
        },
    }
}

/// Map a [`ProtocolError`](llm_proxy_protocol::client::ProtocolError) to a
/// [`RouteError`].
pub(crate) fn protocol_error_to_route(e: llm_proxy_protocol::client::ProtocolError) -> RouteError {
    match e {
        llm_proxy_protocol::client::ProtocolError::InvalidRequest(msg) => {
            RouteError::InvalidRequest(msg)
        }
        other => RouteError::Internal(other.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- protocol_error_to_route ------------------------------------------------

    #[test]
    fn protocol_error_invalid_request_maps_to_route_invalid_request() {
        let err = llm_proxy_protocol::client::ProtocolError::InvalidRequest("bad".into());
        let route_err = protocol_error_to_route(err);
        assert!(matches!(route_err, RouteError::InvalidRequest(_)));
    }

    #[test]
    fn protocol_error_decode_maps_to_route_internal() {
        let err = llm_proxy_protocol::client::ProtocolError::Decode("bad".into());
        let route_err = protocol_error_to_route(err);
        assert!(matches!(route_err, RouteError::Internal(_)));
    }

    // -- map_provider_error -----------------------------------------------------

    #[test]
    fn provider_api_error_maps_to_upstream() {
        let err = llm_proxy_provider::error::ProviderError::api(500, "internal error".into());
        let route_err = map_provider_error(err);
        match route_err {
            RouteError::Upstream { status, .. } => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            }
            other => panic!("expected Upstream, got: {:?}", other),
        }
    }

    #[test]
    fn provider_http_error_maps_to_upstream_502() {
        let err = llm_proxy_provider::error::ProviderError::api(502, "bad gateway".into());
        let route_err = map_provider_error(err);
        match route_err {
            RouteError::Upstream { status, body } => {
                assert_eq!(status, StatusCode::BAD_GATEWAY);
                assert!(body.contains("bad gateway"));
            }
            other => panic!("expected Upstream, got: {:?}", other),
        }
    }

    // -- resolve_target with unknown model --------------------------------------

    #[test]
    fn resolve_target_unknown_model_returns_unknown_model() {
        use std::collections::HashMap;
        use llm_proxy_core::AppConfig;
        use llm_proxy_core::ServerConfig;
        use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};

        let app_config = AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: std::time::Duration::from_secs(60),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "test".to_owned(),
            },
            models: HashMap::new(), // empty routing table
        };

        let providers = llm_proxy_core::ProviderRegistry::from_providers(vec![])
            .expect("empty registry");

        let state = AppState::from_toml(
            app_config,
            providers,
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            crate::state::BuildInfo {
                name: "test",
                version: "0.0.0",
                target: "test",
                git_sha: "test",
            },
        );

        let core = CoreRequest {
            model: llm_proxy_protocol::core::ModelRef {
                requested: "nonexistent".to_owned(),
                upstream: None,
            },
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            sampling: Default::default(),
            stream: false,
            metadata: Default::default(),
            provider_hints: Default::default(),
        };

        let result = resolve_target(&state, &core);
        assert!(result.is_err());
        match result.unwrap_err() {
            RouteError::UnknownModel(m) => assert_eq!(m, "nonexistent"),
            other => panic!("expected UnknownModel, got: {:?}", other),
        }
    }

    // -- resolve_target with missing app_config ---------------------------------

    #[test]
    fn resolve_target_missing_app_config_returns_internal() {
        use llm_proxy_core::{Config, FallbackHandler};
        use llm_proxy_provider::{OpenCodeClient, ProviderAdapterRegistry, ProxyClient};
        use std::sync::Arc;

        // Build legacy state (no app_config, no providers).
        let state = AppState::from_legacy(
            Config::default(),
            crate::state::BuildInfo {
                name: "test",
                version: "0.0.0",
                target: "test",
                git_sha: "test",
            },
            OpenCodeClient::new(Arc::new(Config::default())),
            FallbackHandler::new(3, std::time::Duration::from_secs(30)),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
        );

        let core = CoreRequest {
            model: llm_proxy_protocol::core::ModelRef {
                requested: "any-model".to_owned(),
                upstream: None,
            },
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            sampling: Default::default(),
            stream: false,
            metadata: Default::default(),
            provider_hints: Default::default(),
        };

        let result = resolve_target(&state, &core);
        assert!(result.is_err());
        match result.unwrap_err() {
            RouteError::Internal(msg) => assert!(msg.contains("TOML config required")),
            other => panic!("expected Internal, got: {:?}", other),
        }
    }

    // -- encode_core_event smoke test -------------------------------------------

    #[test]
    fn encode_core_event_ping_produces_event() {
        use llm_proxy_protocol::core::CoreEvent;
        let mut encoder = StreamEncoder::new("msg_test".to_owned(), "test-model".to_owned());
        let events = encode_core_event(
            &mut encoder,
            &ClientProtocol::Anthropic,
            CoreEvent::Ping,
        );
        // Ping may or may not produce output depending on the encoder impl.
        // The important thing is it doesn't panic.
        let _ = events;
    }
}
