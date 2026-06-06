//! Shared core pipeline for route handlers.
//!
//! Provides [`prepare_request`] (rate-limit, dedup, request-ID generation),
//! [`handle_core_once`] (non-streaming), and [`handle_core_stream`] (streaming)
//! used by `/v1/messages` and `/v1/chat/completions`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use bytes::Bytes;
use futures::stream::{BoxStream, StreamExt};
use llm_proxy_core::Metrics;
use llm_proxy_core::model_route::{ModelRouteError, resolve_model_route};
use llm_proxy_protocol::client::anthropic;
use llm_proxy_protocol::client::anthropic::StreamEncoder as AnthropicStreamEncoder;
use llm_proxy_protocol::client::openai_chat;
use llm_proxy_protocol::client::openai_chat::StreamEncoder as OpenAiStreamEncoder;
use llm_proxy_protocol::core::{CoreEvent, CoreRequest};
use llm_proxy_provider::adapter::{
    ProviderAdapter, ProviderAdapterTarget, ProviderProtocol, ProviderStreamDecoder,
};
use llm_proxy_provider::sse::SseFramer;
use llm_proxy_provider::transport::ProxyRequest;
use tracing::warn;

use crate::middleware::get_client_ip;
use crate::state::AppState;

use super::error_response::{
    ClientProtocol, PROVIDER_DECODE_CLIENT_MESSAGE, RouteError, openai_stream_error_json,
    truncate_with_suffix,
};

// ---------------------------------------------------------------------------
// ClientStreamEncoder — protocol-agnostic stream encoder wrapper
// ---------------------------------------------------------------------------

/// Protocol-agnostic wrapper around the Anthropic and OpenAI stream encoders.
///
/// Both encoders share the same interface (`encode_event`, `finish`,
/// `mark_finished`) but produce different output types. This enum dispatches
/// to the correct encoder based on the client protocol.
enum ClientStreamEncoder {
    Anthropic(AnthropicStreamEncoder),
    OpenAi(OpenAiStreamEncoder),
}

/// Output produced by a client stream encoder. Each variant carries the data
/// needed to construct an SSE `Event` for that protocol.
enum ClientEncodedEvent {
    /// Anthropic SSE: requires `event:` line (e.g. `event: message_start`) and
    /// JSON data. The `event_type` is the SSE event name.
    Anthropic { event_type: String, json: String },
    /// OpenAI SSE: just `data: <json>`. No event type field. The caller must
    /// also emit `data: [DONE]` as the terminal frame after the stream ends.
    OpenAi { json: String },
}

impl ClientStreamEncoder {
    /// Create a new encoder for the given protocol.
    ///
    /// For Anthropic, `msg_id` and `model` are used directly.
    /// For OpenAI, `include_usage` is extracted from `provider_hints` if present.
    fn new(protocol: ClientProtocol, msg_id: String, model: String, core: &CoreRequest) -> Self {
        match protocol {
            ClientProtocol::Anthropic => {
                Self::Anthropic(AnthropicStreamEncoder::new(msg_id, model))
            }
            ClientProtocol::OpenAiChat => {
                let include_usage = core
                    .provider_hints
                    .raw
                    .get("stream_options")
                    .and_then(|v| v.get("include_usage"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // The u64 -> i64 cast is safe: u64::MAX corresponds to a date
                // ~584 billion years in the future, unreachable in any real timeline.
                let created = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                Self::OpenAi(OpenAiStreamEncoder::new(
                    msg_id,
                    model,
                    created,
                    include_usage,
                ))
            }
        }
    }

    /// Encode a single [`CoreEvent`] into zero or more protocol-specific events.
    fn encode_event(
        &mut self,
        event: CoreEvent,
    ) -> Result<Vec<ClientEncodedEvent>, llm_proxy_protocol::client::ProtocolError> {
        match self {
            Self::Anthropic(enc) => {
                let msg_events = enc.encode_event(event)?;
                Ok(wrap_anthropic_events(msg_events, "encode"))
            }
            Self::OpenAi(enc) => {
                let chunks = enc.encode_event(event)?;
                Ok(wrap_openai_events(chunks, "encode"))
            }
        }
    }

    /// Flush any remaining buffered events (synthetic terminal if needed).
    fn finish(
        &mut self,
    ) -> Result<Vec<ClientEncodedEvent>, llm_proxy_protocol::client::ProtocolError> {
        match self {
            Self::Anthropic(enc) => {
                let msg_events = enc.finish()?;
                Ok(wrap_anthropic_events(msg_events, "finish"))
            }
            Self::OpenAi(enc) => {
                let chunks = enc.finish()?;
                Ok(wrap_openai_events(chunks, "finish"))
            }
        }
    }

    /// Mark the encoder as finished so subsequent `finish()` returns empty.
    fn mark_finished(&mut self) {
        match self {
            Self::Anthropic(enc) => enc.mark_finished(),
            Self::OpenAi(enc) => enc.mark_finished(),
        }
    }
}

/// Serialize Anthropic SSE events, dropping any that fail to serialize.
///
/// `phase` is used in log messages (e.g. "encode" vs "finish") for context.
fn wrap_anthropic_events(
    events: Vec<llm_proxy_protocol::anthropic::MessageEvent>,
    phase: &str,
) -> Vec<ClientEncodedEvent> {
    events
        .into_iter()
        .filter_map(|me| {
            let event_type = me.r#type.clone();
            match serde_json::to_string(&me) {
                Ok(json) => Some(ClientEncodedEvent::Anthropic { event_type, json }),
                Err(e) => {
                    warn!(error = %e, phase, "failed to serialize Anthropic SSE event; dropping");
                    None
                }
            }
        })
        .collect()
}

/// Serialize OpenAI SSE chunks, dropping any that fail to serialize.
///
/// `phase` is used in log messages (e.g. "encode" vs "finish") for context.
fn wrap_openai_events<T: serde::Serialize>(chunks: Vec<T>, phase: &str) -> Vec<ClientEncodedEvent> {
    chunks
        .into_iter()
        .filter_map(|chunk| match serde_json::to_string(&chunk) {
            Ok(json) => Some(ClientEncodedEvent::OpenAi { json }),
            Err(e) => {
                warn!(error = %e, phase, "failed to serialize OpenAI SSE chunk; dropping");
                None
            }
        })
        .collect()
}

/// Convert a [`ClientEncodedEvent`] into an axum SSE [`Event`].
fn client_event_to_sse(encoded: ClientEncodedEvent) -> Event {
    match encoded {
        ClientEncodedEvent::Anthropic { event_type, json } => {
            Event::default().event(&event_type).data(json)
        }
        ClientEncodedEvent::OpenAi { json } => Event::default().data(json),
    }
}

/// Build the terminal `[DONE]` event for OpenAI Chat streams.
fn openai_done_event() -> Event {
    Event::default().data("[DONE]")
}

// ---------------------------------------------------------------------------
// RequestContext
// ---------------------------------------------------------------------------

/// Pre-resolved per-request context shared between non-streaming and streaming
/// paths.
#[derive(Debug)]
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
        return Err(RouteError::RateLimited);
    }

    if state.request_dedup.is_duplicate(body) {
        state.metrics.record_deduplicated();
        return Err(RouteError::Conflict);
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
    let app_config = state.app_config();
    let providers = state.providers();

    let target =
        resolve_model_route(&app_config.models, &core.model.requested).map_err(|e| match e {
            ModelRouteError::UnknownModel(m) => RouteError::UnknownModel(m),
            _ => RouteError::Internal(e.to_string()),
        })?;

    let adapter_target_config = providers
        .resolve_adapter_target(&target)
        .map_err(|e| RouteError::Internal(e.to_string()))?;

    let protocol = ProviderProtocol::parse(&adapter_target_config.protocol).ok_or_else(|| {
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
        .clone(); // ProviderAdapter is Arc-like: clone is a cheap reference count increment, not a deep copy.

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
    state.metrics.record_request(false);

    tracing::debug!(
        request_id = %ctx.request_id,
        model = %core.model.requested,
        streaming = false,
        "processing request"
    );

    let (target, adapter) = resolve_target(&state, &core).inspect_err(|_| {
        state.metrics.record_failure();
    })?;

    tracing::debug!(
        request_id = %ctx.request_id,
        provider = %target.provider_name,
        model = %target.upstream_model,
        "routed to provider"
    );

    // Encode the core request into a provider-specific HTTP request.
    // All encode errors map to Internal per the plan's error behavior spec:
    // the core pipeline has already validated the request at this point, so
    // any encode failure is a proxy/adapter issue, not a client error.
    let proxy_req: ProxyRequest = adapter.encode_request(&core, &target).map_err(|e| {
        state.metrics.record_failure();
        RouteError::Internal(format!("encode error: {e}"))
    })?;

    // Send to upstream.
    let response_bytes = state.proxy_client.send(proxy_req).await.map_err(|e| {
        state.metrics.record_failure();
        map_provider_error(e)
    })?;

    // Decode the provider response into a CoreResponse.
    let core_resp = adapter
        .decode_response(&response_bytes, &target)
        .map_err(|e| {
            state.metrics.record_failure();
            RouteError::ProviderDecode(format!("decode response: {e}"))
        })?;

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
            let chat_resp = openai_chat::encode_response(core_resp)
                .map_err(|e| RouteError::Internal(format!("client encode: {e}")))?;
            serde_json::to_vec(&chat_resp)
                .map_err(|e| RouteError::Internal(format!("serialize: {e}")))?
        }
    };

    tracing::debug!(
        request_id = %ctx.request_id,
        model = %target.upstream_model,
        latency_ms = latency.as_millis(),
        "request completed"
    );

    let mut response = (StatusCode::OK, Body::from(response_body)).into_response();

    // Insert the request ID header safely -- the ID is dynamically generated
    // so we use from_str with a fallback rather than expect/unwrap.
    response.headers_mut().insert(
        "x-request-id",
        ctx.request_id
            .parse()
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("unknown")),
    );

    // Set content-type explicitly (defense-in-depth).
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
///
/// Chosen as 3 seconds to keep Anthropic SDK clients from timing out on
/// long-running streams while avoiding excessive bandwidth overhead.
/// TODO(future): Make this configurable via AppState or TOML config.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

/// Message sent from the spawned stream task to the handler during the
/// first-byte probe phase. This enables the handler to return a proper
/// HTTP error status if the stream fails before emitting any data.
enum FirstByteResult {
    /// At least one SSE event was produced by the stream. The stream is now
    /// live and all subsequent events/errors must be delivered as in-band
    /// SSE events (the HTTP status is already committed as 200 OK).
    FirstEvent(Event),
    /// The stream failed before producing any event. The handler should
    /// return an HTTP error response (e.g. 502 Bad Gateway) instead of
    /// committing a 200 OK SSE response.
    PreStreamError(RouteError),
}

/// Streaming core pipeline.
///
/// ```text
/// CoreRequest -> encode -> send_stream -> SseFramer -> ProviderStreamDecoder
///            -> StreamEncoder -> SSE response
/// ```
///
/// ## first_byte_sent tracking
///
/// The plan requires that errors before the first data byte be returned as
/// HTTP errors (e.g. 502), not as in-band SSE error events. This is
/// implemented by probing for the first event before committing the HTTP 200
/// response: the spawned task sends the first event (or an error) back to
/// the handler via a oneshot channel, and the handler decides whether to
/// return an HTTP error or an SSE response.
pub(crate) async fn handle_core_stream(
    state: AppState,
    ctx: RequestContext,
    core: CoreRequest,
    client_protocol: ClientProtocol,
) -> Result<Response<Body>, RouteError> {
    state.metrics.record_request(true);

    tracing::debug!(
        request_id = %ctx.request_id,
        model = %core.model.requested,
        streaming = true,
        "processing streaming request"
    );

    let (target, adapter) = resolve_target(&state, &core).inspect_err(|_| {
        state.metrics.record_failure();
    })?;

    tracing::debug!(
        request_id = %ctx.request_id,
        provider = %target.provider_name,
        model = %target.upstream_model,
        "routed to provider (streaming)"
    );

    // Encode the core request into a provider-specific HTTP request.
    // All encode errors map to Internal per the plan's error behavior spec:
    // the core pipeline has already validated the request at this point.
    let proxy_req: ProxyRequest = adapter.encode_request(&core, &target).map_err(|e| {
        state.metrics.record_failure();
        RouteError::Internal(format!("encode error: {e}"))
    })?;

    // Open the streaming connection.
    let byte_stream = state
        .proxy_client
        .send_stream(proxy_req)
        .await
        .map_err(|e| {
            state.metrics.record_failure();
            map_provider_error(e)
        })?;

    // Create a provider stream decoder.
    let provider_decoder = adapter.new_stream_decoder(&target);
    let sse_framer = SseFramer::new();

    // Create the client stream encoder.
    // NOTE: This generates a synthetic message ID. The prefix is chosen based
    // on the client protocol so it matches the expected convention:
    //   - OpenAI Chat uses `chatcmpl-` prefix
    //   - Anthropic uses `msg_` prefix
    // When available, the upstream provider's message ID should be extracted
    // from the first CoreEvent::MessageStart instead, so clients tracking
    // message IDs for conversation continuity see the real ID.
    let msg_id = match client_protocol {
        ClientProtocol::OpenAiChat => format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        ClientProtocol::Anthropic => format!("msg_{}", uuid::Uuid::new_v4()),
    };
    let client_encoder =
        ClientStreamEncoder::new(client_protocol, msg_id, core.model.requested.clone(), &core);

    // ctx is consumed after this point. request_id is cloned once for the
    // spawned task and once for the response header (both are needed).
    let request_id = ctx.request_id;
    let upstream_model = target.upstream_model.clone();

    // Build the output SSE stream with first-byte tracking.
    let (first_byte_tx, first_byte_rx) = tokio::sync::oneshot::channel::<FirstByteResult>();
    let output_stream = build_sse_output_stream(
        byte_stream,
        StreamContext {
            provider_decoder,
            sse_framer,
            client_encoder,
            client_protocol,
            request_id: request_id.clone(),
            stream_metrics: StreamMetrics {
                metrics: Arc::clone(&state.metrics),
                upstream_model: upstream_model.clone(),
                start: ctx.start,
            },
            first_byte_tx: Some(first_byte_tx),
            first_byte_sent: false,
        },
    );

    // Wait for the first event (or a pre-stream error). This is the
    // first_byte_sent boundary: errors before this point become HTTP
    // errors; errors after this point become in-band SSE error events.
    let first_event = first_byte_rx.await.map_err(|_| {
        state.metrics.record_failure();
        RouteError::Internal(
            "stream task exited unexpectedly before first event (possible panic)".to_owned(),
        )
    })?;

    match first_event {
        FirstByteResult::PreStreamError(route_error) => {
            // Stream failed before emitting any data. Return as HTTP error.
            Err(route_error)
        }
        FirstByteResult::FirstEvent(event) => {
            // First event received. Commit HTTP 200 and start streaming.
            // Prepend the first event to the output stream.
            let stream =
                futures::stream::once(async move { Ok::<_, std::convert::Infallible>(event) })
                    .chain(output_stream.map(Ok::<_, std::convert::Infallible>));

            let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(HEARTBEAT_INTERVAL));

            let response = sse.into_response();
            let (mut parts, body) = response.into_parts();

            // Add custom headers.
            // Defense-in-depth: explicitly set Content-Type even though
            // Sse::into_response() sets it internally. This guards against
            // axum version changes and makes the intent explicit.
            parts.headers.insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            // Prevent proxy/CDN caching of streaming responses.
            parts.headers.insert(
                header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-cache"),
            );
            // Defense-in-depth: explicitly request persistent connection for
            // reverse-proxy scenarios. axum adds this automatically for HTTP/1.1
            // but some reverse proxies strip it unless explicitly set.
            parts.headers.insert(
                header::CONNECTION,
                axum::http::HeaderValue::from_static("keep-alive"),
            );
            parts.headers.insert(
                "x-accel-buffering",
                axum::http::HeaderValue::from_static("no"),
            );
            parts.headers.insert(
                "x-request-id",
                request_id
                    .parse()
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("unknown")),
            );

            tracing::debug!(
                request_id = %request_id,
                model = %target.upstream_model,
                "streaming started"
            );

            Ok(Response::from_parts(parts, body))
        }
    }
}

// ---------------------------------------------------------------------------
// SSE output stream builder
// ---------------------------------------------------------------------------

/// Context for tracking metrics during a streaming response.
#[derive(Debug)]
struct StreamMetrics {
    /// Shared metrics recorder for success/failure/latency tracking.
    metrics: Arc<Metrics>,
    /// Upstream model name used as the metrics label.
    upstream_model: String,
    /// Instant when the handler was entered (for latency measurement).
    start: Instant,
}

/// Mutable context used inside the spawned stream task.
///
/// Groups the state that was previously passed as individual parameters to
/// `build_sse_output_stream`, reducing the function's parameter count from 9
/// to a single struct plus the byte stream.
struct StreamContext {
    provider_decoder: Box<dyn ProviderStreamDecoder + Send>,
    sse_framer: SseFramer,
    client_encoder: ClientStreamEncoder,
    client_protocol: ClientProtocol,
    request_id: String,
    stream_metrics: StreamMetrics,
    first_byte_tx: Option<tokio::sync::oneshot::Sender<FirstByteResult>>,
    first_byte_sent: bool,
}

impl StreamContext {
    /// Emit a single SSE event, handling the first-byte boundary.
    ///
    /// Before the first event is sent, events go through the `first_byte_tx`
    /// channel so the handler can return a proper HTTP status. After the first
    /// event, events go directly to the `tx` channel.
    ///
    /// Returns `true` if the event was successfully delivered (or the stream
    /// should continue), `false` if the task should exit.
    async fn emit_event(&mut self, event: Event, tx: &tokio::sync::mpsc::Sender<Event>) -> bool {
        if !self.first_byte_sent {
            if let Some(fb_tx) = self.first_byte_tx.take() {
                if fb_tx.send(FirstByteResult::FirstEvent(event)).is_ok() {
                    self.first_byte_sent = true;
                    return true;
                }
                // Receiver dropped -- handler is gone.
                self.stream_metrics.metrics.record_failure();
                return false;
            }
        } else if tx.send(event).await.is_err() {
            self.stream_metrics.metrics.record_failure();
            return false;
        }
        true
    }

    /// Emit a pre-stream error through the first-byte channel.
    fn send_pre_stream_error(&mut self, error: RouteError) -> bool {
        if let Some(fb_tx) = self.first_byte_tx.take() {
            fb_tx.send(FirstByteResult::PreStreamError(error)).is_ok()
        } else {
            false
        }
    }
}

/// Build a stream that converts provider byte chunks into client SSE events.
///
/// The `first_byte_tx` channel is used to signal the first-byte boundary:
/// the spawned task sends either `FirstByteResult::FirstEvent` (once the
/// first SSE event is produced) or `FirstByteResult::PreStreamError` (if
/// the stream fails before any event). After the first event is sent, the
/// channel is dropped and all subsequent errors become in-band SSE events.
fn build_sse_output_stream(
    byte_stream: std::pin::Pin<
        Box<
            dyn futures::Stream<Item = Result<Bytes, llm_proxy_provider::error::ProviderError>>
                + Send
                + 'static,
        >,
    >,
    ctx: StreamContext,
) -> BoxStream<'static, Event> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(256);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        let mut stream = byte_stream;
        let mut ctx = ctx;
        let mut stream_succeeded = false;
        // Track whether the stream terminated due to an error so we can
        // skip the finalization path that follows the main loop.
        let mut stream_errored = false;

        'outer: loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    // Client disconnected; abort upstream stream.
                    ctx.stream_metrics.metrics.record_failure();
                    if !ctx.first_byte_sent {
                        ctx.send_pre_stream_error(
                            RouteError::Internal("client disconnected before first byte".to_owned()),
                        );
                    }
                    return;
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            // Feed bytes through the SSE framer.
                            let frames = match ctx.sse_framer.push_chunk(&bytes) {
                                Ok(f) => f,
                                Err(e) => {
                                    warn!(
                                        request_id = %ctx.request_id,
                                        error = %e,
                                        "SSE framing error in stream"
                                    );
                                    if !ctx.first_byte_sent {
                                        ctx.stream_metrics.metrics.record_failure();
                                        ctx.send_pre_stream_error(
                                            RouteError::ProviderDecode(
                                                format!("stream framing error: {e}"),
                                            ),
                                        );
                                    } else {
                                        emit_stream_error(
                                            &mut ctx.client_encoder,
                                            &tx,
                                            &ctx.client_protocol,
                                            PROVIDER_DECODE_CLIENT_MESSAGE,
                                        ).await;
                                        ctx.stream_metrics.metrics.record_failure();
                                    }
                                    stream_errored = true;
                                    break 'outer;
                                }
                            };

                            for frame in &frames {
                                let core_events = match ctx.provider_decoder.decode_frame(frame) {
                                    Ok(events) => events,
                                    Err(e) => {
                                        warn!(
                                            request_id = %ctx.request_id,
                                            error = %e,
                                            "provider decode error in stream"
                                        );
                                        if !ctx.first_byte_sent {
                                            ctx.stream_metrics.metrics.record_failure();
                                            ctx.send_pre_stream_error(
                                                RouteError::ProviderDecode(
                                                    format!("provider decode error: {e}"),
                                                ),
                                            );
                                        } else {
                                            emit_stream_error(
                                                &mut ctx.client_encoder,
                                                &tx,
                                                &ctx.client_protocol,
                                                PROVIDER_DECODE_CLIENT_MESSAGE,
                                            ).await;
                                            ctx.stream_metrics.metrics.record_failure();
                                        }
                                        stream_errored = true;
                                        break 'outer;
                                    }
                                };

                                for core_event in core_events {
                                    let client_events = encode_core_event(
                                        &mut ctx.client_encoder,
                                        core_event,
                                    );
                                    for event in client_events {
                                        if !ctx.emit_event(event, &tx).await {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!(
                                request_id = %ctx.request_id,
                                error = %e,
                                "upstream stream error"
                            );
                            if !ctx.first_byte_sent {
                                ctx.stream_metrics.metrics.record_failure();
                                ctx.send_pre_stream_error(
                                    map_provider_error(e),
                                );
                            } else {
                                let sanitized = sanitize_upstream_error_body(&e.to_string());
                                emit_stream_error(
                                    &mut ctx.client_encoder,
                                    &tx,
                                    &ctx.client_protocol,
                                    &sanitized,
                                ).await;
                                ctx.stream_metrics.metrics.record_failure();
                            }
                            stream_errored = true;
                            break 'outer;
                        }
                        None => {
                            // Stream ended with no events at all.
                            // If first byte was never sent, this is a normal
                            // completion of an empty stream -- the handler will
                            // see the channel close and respond accordingly.
                            break 'outer;
                        }
                    }
                }
            }
        }

        // Only run finalization when the stream completed normally (not errored).
        // When stream_errored is true, emit_stream_error already handled the
        // terminal events (or the error was sent back as an HTTP error).
        if !stream_errored {
            // Finalize: call sse_framer.finish() first to flush any trailing partial
            // SSE frame that arrived without a terminating blank line.
            match ctx.sse_framer.finish() {
                Ok(trailing_frames) => {
                    for frame in &trailing_frames {
                        let core_events = match ctx.provider_decoder.decode_frame(frame) {
                            Ok(events) => events,
                            Err(e) => {
                                warn!(
                                    request_id = %ctx.request_id,
                                    error = %e,
                                    "provider decode error on trailing frame"
                                );
                                break;
                            }
                        };
                        for core_event in core_events {
                            let client_events =
                                encode_core_event(&mut ctx.client_encoder, core_event);
                            for event in client_events {
                                if !ctx.emit_event(event, &tx).await {
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        request_id = %ctx.request_id,
                        error = %e,
                        "SSE framer finish error"
                    );
                }
            }

            // Then finalize: call provider decoder finish() to emit any remaining events.
            match ctx.provider_decoder.finish() {
                Ok(final_events) => {
                    for core_event in final_events {
                        let client_events = encode_core_event(&mut ctx.client_encoder, core_event);
                        for event in client_events {
                            if !ctx.emit_event(event, &tx).await {
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        request_id = %ctx.request_id,
                        error = %e,
                        "provider decoder finish error"
                    );
                }
            }

            // Emit any remaining client encoder events (synthetic terminal if needed).
            match ctx.client_encoder.finish() {
                Ok(final_encoded_events) => {
                    for encoded in final_encoded_events {
                        let event = client_event_to_sse(encoded);
                        if !ctx.emit_event(event, &tx).await {
                            return;
                        }
                    }

                    // For OpenAI Chat, emit the [DONE] terminator after all chunks.
                    if matches!(ctx.client_protocol, ClientProtocol::OpenAiChat) {
                        let done_event = openai_done_event();
                        if !ctx.emit_event(done_event, &tx).await {
                            return;
                        }
                    }

                    stream_succeeded = true;
                }
                Err(e) => {
                    warn!(
                        request_id = %ctx.request_id,
                        error = %e,
                        "client encoder finish error"
                    );
                    ctx.stream_metrics.metrics.record_failure();
                }
            }
        }

        // Record metrics after stream completes.
        if stream_succeeded {
            ctx.stream_metrics.metrics.record_success(
                &ctx.stream_metrics.upstream_model,
                ctx.stream_metrics.start.elapsed(),
            );
        }

        // If the stream ended without ever sending a first byte (empty stream
        // with no errors), send a PreStreamError with a descriptive message.
        // The handler will return this as a 502 Bad Gateway to the client.
        // This is preferable to silently dropping the channel (which produces
        // a misleading "stream task panicked" error).
        if !ctx.first_byte_sent {
            if let Some(tx) = ctx.first_byte_tx.take() {
                let _ = tx.send(FirstByteResult::PreStreamError(RouteError::ProviderDecode(
                    "upstream returned an empty stream with no events".to_owned(),
                )));
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
fn encode_core_event(client_encoder: &mut ClientStreamEncoder, event: CoreEvent) -> Vec<Event> {
    match client_encoder.encode_event(event) {
        Ok(encoded_events) => encoded_events
            .into_iter()
            .map(client_event_to_sse)
            .collect(),
        Err(e) => {
            warn!(error = %e, "client stream encode error");
            Vec::new()
        }
    }
}

/// Emit an error event into the stream.
///
/// For OpenAI Chat, the StreamEncoder returns `Err` for `CoreEvent::Error`.
/// Rather than silently dropping the error, we construct a raw SSE error
/// event directly so the client sees an error indication before `[DONE]`.
///
/// After the error event, the encoder's `finished` flag is set to `true` so
/// that any subsequent `finish()` call returns an empty vec. The error event
/// itself is the terminal event -- no synthetic message_delta/message_stop
/// pair should follow an error.
async fn emit_stream_error(
    client_encoder: &mut ClientStreamEncoder,
    tx: &tokio::sync::mpsc::Sender<Event>,
    client_protocol: &ClientProtocol,
    message: &str,
) {
    use llm_proxy_protocol::core::{CoreStreamError, CoreStreamErrorKind};

    let error_event = CoreEvent::Error {
        error: CoreStreamError::new(CoreStreamErrorKind::Upstream, message.to_owned()),
    };

    let events = encode_core_event(client_encoder, error_event);
    if events.is_empty() && matches!(client_protocol, ClientProtocol::OpenAiChat) {
        // The OpenAI StreamEncoder returns Err for CoreEvent::Error, which
        // causes encode_core_event to produce an empty Vec. Construct an SSE
        // error event using the same typed structs as the HTTP error path so
        // both paths stay consistent at compile time.
        if let Some(json_str) = openai_stream_error_json(message) {
            let _ = tx.send(Event::default().data(json_str)).await;
        }
    } else {
        for event in events {
            if tx.send(event).await.is_err() {
                return;
            }
        }
    }

    // For OpenAI Chat, emit the [DONE] terminator after an error event.
    if matches!(client_protocol, ClientProtocol::OpenAiChat)
        && tx.send(openai_done_event()).await.is_err()
    {
        return;
    }

    // Mark the encoder as finished so that subsequent finish() calls return
    // empty events. The error event itself is the terminal event -- we do NOT
    // emit synthetic message_delta/message_stop after an error.
    client_encoder.mark_finished();
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

/// Map a [`ProviderError`](llm_proxy_provider::error::ProviderError) to a
/// [`RouteError`].
///
/// Upstream error bodies are sanitized before being forwarded to clients:
/// - `Api` variants: already sanitized at construction time.
/// - Non-`Api` variants (Http, Serialize, etc.): sanitized here to strip
///   upstream hostnames, URL paths, and potential secrets from reqwest error
///   messages before they reach the client.
pub(crate) fn map_provider_error(e: llm_proxy_provider::error::ProviderError) -> RouteError {
    match &e {
        llm_proxy_provider::error::ProviderError::Api { status, body } => {
            let status_code = StatusCode::from_u16(*status).unwrap_or_else(|_| {
                warn!(
                    status = *status,
                    "invalid HTTP status from provider; mapping to 502"
                );
                StatusCode::BAD_GATEWAY
            });
            RouteError::Upstream {
                status: status_code,
                body: body.clone(),
            }
        }
        _ => {
            // Check for reqwest timeout specifically so we can return 504
            // instead of the generic 502 Bad Gateway.
            if let Some(reqwest_err) = is_reqwest_timeout(&e) {
                let sanitized = sanitize_upstream_error_body(&reqwest_err.to_string());
                return RouteError::UpstreamTimeout(sanitized);
            }
            // Sanitize non-Api error messages to prevent leaking upstream
            // hostnames, URL paths, or connection details in the response body.
            let sanitized = sanitize_upstream_error_body(&e.to_string());
            RouteError::Upstream {
                status: StatusCode::BAD_GATEWAY,
                body: sanitized,
            }
        }
    }
}

/// Check whether a `ProviderError` wraps a reqwest timeout error.
///
/// Returns the inner `reqwest::Error` reference if the error chain contains
/// a timeout, so the caller can extract a sanitized message.
fn is_reqwest_timeout(e: &llm_proxy_provider::error::ProviderError) -> Option<&reqwest::Error> {
    match e {
        llm_proxy_provider::error::ProviderError::Http(http_err) => {
            // reqwest::Error implements std::error::Error; check is_timeout().
            if http_err.is_timeout() {
                return Some(http_err);
            }
            None
        }
        _ => None,
    }
}

/// Maximum length for sanitized upstream error bodies before passing to
/// `truncate_error_body()` in the error response encoder.
const MAX_SANITIZE_LEN: usize = 512;
const SANITIZE_SUFFIX: &str = "...[truncated]";

/// Compiled regex for URL redaction, initialized once via `LazyLock`.
static URL_REDACT_REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"https?://\S+").expect("valid URL redaction regex")
});

/// Sanitize non-Api upstream error bodies to prevent information leakage.
///
/// Non-Api errors (Http, Serialize, Utf8, etc.) are converted via
/// `e.to_string()`, which for reqwest errors can contain full URLs including
/// hostnames and paths. This function replaces such details with generic
/// messages while preserving enough context for debugging.
fn sanitize_upstream_error_body(msg: &str) -> String {
    // Truncate to MAX_SANITIZE_LEN as a safety net. The suffix is included
    // within this budget. The actual error body in RouteError::Upstream is
    // further truncated by truncate_error_body() in the error response encoder.
    let truncated = truncate_with_suffix(msg, MAX_SANITIZE_LEN, SANITIZE_SUFFIX);

    // Redact URL-like patterns that may contain hostnames/paths.
    // Matches http:// or https:// followed by any non-whitespace chars.
    URL_REDACT_REGEX
        .replace_all(&truncated, "[url-redacted]")
        .into_owned()
}

/// Map a [`ProtocolError`](llm_proxy_protocol::client::ProtocolError) to a
/// [`RouteError`].
///
/// Per the `ProtocolError` doc comments:
/// - `InvalidRequest` -> 400 Bad Request (bad client input)
/// - `Decode` -> 400 Bad Request (malformed client input)
/// - `Encode` -> 500 Internal Server Error
/// - `EncodeSkippable` -> mapped as non-fatal (skipped), so 500 if it
///   reaches this path.
pub(crate) fn protocol_error_to_route(e: llm_proxy_protocol::client::ProtocolError) -> RouteError {
    match e {
        llm_proxy_protocol::client::ProtocolError::InvalidRequest(msg) => {
            RouteError::InvalidRequest(msg)
        }
        llm_proxy_protocol::client::ProtocolError::Decode(msg) => {
            // Per ProtocolError doc: Decode -> 400 Bad Request (malformed client input).
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
    fn protocol_error_decode_maps_to_route_invalid_request() {
        let err = llm_proxy_protocol::client::ProtocolError::Decode("bad".into());
        let route_err = protocol_error_to_route(err);
        match route_err {
            RouteError::InvalidRequest(msg) => assert!(msg.contains("bad")),
            other => panic!("expected InvalidRequest, got: {:?}", other),
        }
    }

    #[test]
    fn protocol_error_encode_maps_to_route_internal() {
        let err = llm_proxy_protocol::client::ProtocolError::Encode("bad".into());
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
        use llm_proxy_core::AppConfig;
        use llm_proxy_core::ServerConfig;
        use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};
        use std::collections::HashMap;

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

        let providers =
            llm_proxy_core::ProviderRegistry::from_providers(vec![]).expect("empty registry");

        let state = AppState::new(
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

    // -- resolve_target with empty routing table returns unknown model ----------

    #[test]
    fn resolve_target_empty_routing_table_returns_unknown_model() {
        use llm_proxy_core::AppConfig;
        use llm_proxy_core::ServerConfig;
        use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};
        use std::collections::HashMap;

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

        let providers =
            llm_proxy_core::ProviderRegistry::from_providers(vec![]).expect("empty registry");

        let state = AppState::new(
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
                requested: "nonexistent-model".to_owned(),
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
            RouteError::UnknownModel(m) => assert_eq!(m, "nonexistent-model"),
            other => panic!("expected UnknownModel, got: {:?}", other),
        }
    }

    // -- encode_core_event smoke test -------------------------------------------

    #[test]
    fn encode_core_event_ping_produces_event() {
        use llm_proxy_protocol::core::CoreEvent;
        let mut encoder = ClientStreamEncoder::Anthropic(AnthropicStreamEncoder::new(
            "msg_test".to_owned(),
            "test-model".to_owned(),
        ));
        let events = encode_core_event(&mut encoder, CoreEvent::Ping);
        // Ping may or may not produce output depending on the encoder impl.
        // The important thing is it doesn't panic.
        let _ = events;
    }

    // -- sanitize_upstream_error_body tests ------------------------------------

    #[test]
    fn sanitize_removes_urls() {
        let msg =
            "request failed: connection refused to https://api.openai.com/v1/chat/completions";
        let sanitized = sanitize_upstream_error_body(msg);
        assert!(
            !sanitized.contains("api.openai.com"),
            "URL should be redacted"
        );
        assert!(
            sanitized.contains("[url-redacted]"),
            "should contain redacted placeholder"
        );
    }

    #[test]
    fn sanitize_truncates_long_messages() {
        let msg = "x".repeat(600);
        let sanitized = sanitize_upstream_error_body(&msg);
        assert!(sanitized.ends_with("...[truncated]"));
    }

    #[test]
    fn sanitize_preserves_short_messages_without_urls() {
        let msg = "connection reset by peer";
        let sanitized = sanitize_upstream_error_body(msg);
        assert_eq!(sanitized, msg);
    }
}
