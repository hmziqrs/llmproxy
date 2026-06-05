//! Main /v1/messages proxy handler.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Response, StatusCode, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream::{BoxStream, StreamExt};
use llm_proxy_core::{
    MessageContent, ModelConfig,
    router::{Scenario, ScenarioConfig, detect_scenario, route_for_streaming},
};
use llm_proxy_protocol::{
    anthropic::MessageRequest,
    transformer::{
        request::{transform_request, transform_to_gemini, transform_to_responses},
        response::{transform_gemini_response, transform_response, transform_responses_response},
        stream::StreamProxy,
    },
};
use llm_proxy_provider::{EndpointType, OpenCodeClient, ProviderError, classify_endpoint};
use tracing::{error, info, warn};

use crate::error::{ApiError, ApiErrorWithRequestId};
use crate::middleware::get_client_ip;
use crate::state::AppState;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, ApiErrorWithRequestId> {
    let start = Instant::now();
    let request_id = state.request_id_gen.next_id();

    let client_ip = get_client_ip(&headers, None);
    if !state.rate_limiter.is_allowed(&client_ip) {
        state.metrics.record_rate_limited();
        // Note: client_ip is intentionally excluded from the client-facing
        // error message to avoid leaking internal network topology. The IP
        // is logged server-side for rate-limit monitoring.
        return Err(ApiError::RateLimited(
            "rate limit exceeded".to_owned(),
        ).with_request_id(request_id));
    }

    if state.request_dedup.is_duplicate(&body) {
        state.metrics.record_deduplicated();
        return Err(ApiError::Duplicate(
            "duplicate request, please retry".to_owned(),
        ).with_request_id(request_id));
    }

    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::BadRequest(format!("invalid JSON: {e}"))
            .with_request_id(request_id.clone()))?;

    req.validate()
        .map_err(|e| ApiError::BadRequest(e).with_request_id(request_id.clone()))?;

    let is_streaming = req.stream.unwrap_or(false);
    // Note: Metrics are recorded after rate-limit and dedup checks, so
    // rate-limited and duplicate requests are not counted. The metric name
    // `requests_received` suggests all inbound traffic but actually counts
    // only requests that proceed to processing. Consider renaming to
    // `requests_processed` for accuracy, or moving the call before rate-limit.
    state.metrics.record_request(is_streaming);

    info!(request_id = %request_id, model = %req.model, streaming = is_streaming, "processing request");

    let scenario = detect_scenario_from_request(&req, &state, is_streaming);
    let model = state
        .model_router
        .resolve(&scenario.to_string())
        .ok_or_else(|| {
            ApiError::Internal(format!("no model configured for scenario: {scenario}"))
                .with_request_id(request_id.clone())
        })?;

    info!(request_id = %request_id, scenario = %scenario, model = %model.model_id, "routed to model");

    let fallback_chain = {
        let mut chain = vec![model.clone()];
        if let Some(fb) = state.model_router.fallback_chain(&scenario.to_string()) {
            chain.extend(fb.iter().cloned());
        }
        chain
    };

    if is_streaming {
        handle_streaming(&state, request_id, req, fallback_chain, start).await
    } else {
        handle_non_streaming(&state, request_id, req, fallback_chain, start).await
    }
}

fn detect_scenario_from_request(
    req: &MessageRequest,
    state: &AppState,
    streaming: bool,
) -> Scenario {
    let system_text = req.system_text();
    let messages: Vec<MessageContent> = req
        .messages
        .iter()
        .map(|msg| {
            let text: String = msg
                .content_blocks()
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

    let token_count = state.token_counter.count_messages(&system_text, &messages) as i32;
    let cfg = build_scenario_config(&state.config);

    if streaming {
        route_for_streaming(&messages, token_count, Some(&cfg)).scenario
    } else {
        detect_scenario(&messages, token_count, Some(&cfg)).scenario
    }
}

fn build_scenario_config(config: &llm_proxy_core::Config) -> ScenarioConfig {
    let threshold = config
        .models
        .get("long_context")
        .map(|m| m.context_threshold as i32)
        .unwrap_or(100_000);
    match config
        .models
        .get("long_context")
        .map(|m| m.model_id.clone())
    {
        Some(mid) => ScenarioConfig::with_model(threshold, mid),
        None => ScenarioConfig::new(threshold),
    }
}

async fn handle_non_streaming(
    state: &AppState,
    request_id: String,
    req: MessageRequest,
    models: Vec<ModelConfig>,
    start: Instant,
) -> Result<Response<Body>, ApiErrorWithRequestId> {
    let client = Arc::clone(&state.client);
    let metrics = Arc::clone(&state.metrics);

    let (result, response_bytes) = state
        .fallback_handler
        .execute_with_fallback(&models, |model| {
            let req = req.clone();
            let client = Arc::clone(&client);
            let model = model.clone();
            async move { execute_non_streaming_request(&client, &req, &model).await }
        })
        .await;

    let latency = start.elapsed();

    if result.success {
        if let Some(bytes) = response_bytes {
            metrics.record_success(&result.model_id, latency);
            info!(request_id = %request_id, model = %result.model_id, latency_ms = latency.as_millis(), "request completed");
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-request-id", &request_id)
                .body(Body::from(bytes))
                .expect("static header values are valid"))
        } else {
            metrics.record_failure();
            Err(ApiError::Internal("no response body".to_owned())
                .with_request_id(request_id))
        }
    } else {
        metrics.record_failure();
        error!(request_id = %request_id, error = %result.error.as_deref().unwrap_or("unknown"), attempted = result.attempted, "all models failed");
        Err(ApiError::Upstream(sanitize_upstream_error(
            result
                .error
                .unwrap_or_else(|| "all models failed".to_owned()),
        )).with_request_id(request_id))
    }
}

async fn execute_non_streaming_request(
    client: &OpenCodeClient,
    req: &MessageRequest,
    model: &ModelConfig,
) -> Result<Vec<u8>, String> {
    match classify_endpoint(&model.model_id) {
        EndpointType::Anthropic => {
            let b = serde_json::to_vec(req).map_err(|e| format!("ser: {e}"))?;
            let resp = client
                .send_anthropic_request(&b, false, model)
                .await
                .map_err(|e| format_provider_error("anth", e))?;
            Ok(resp
                .bytes()
                .await
                .map_err(|e| format!("body: {e}"))?
                .to_vec())
        }
        EndpointType::ChatCompletions => {
            let r = transform_request(req, model).map_err(|e| format!("tf: {e}"))?;
            let resp = client
                .chat_completion_non_streaming(&model.model_id, r, model)
                .await
                .map_err(|e| format_provider_error("chat", e))?;
            let a = transform_response(&resp, &model.model_id).map_err(|e| format!("resp: {e}"))?;
            serde_json::to_vec(&a).map_err(|e| format!("ser: {e}"))
        }
        EndpointType::Responses => {
            let r = transform_to_responses(req, model).map_err(|e| format!("tf: {e}"))?;
            let resp = client
                .responses_completion_non_streaming(&model.model_id, r, model)
                .await
                .map_err(|e| format_provider_error("resp", e))?;
            let a = transform_responses_response(&resp, &model.model_id)
                .map_err(|e| format!("resp: {e}"))?;
            serde_json::to_vec(&a).map_err(|e| format!("ser: {e}"))
        }
        EndpointType::Gemini => {
            let r = transform_to_gemini(req, model).map_err(|e| format!("tf: {e}"))?;
            let resp = client
                .gemini_completion_non_streaming(&model.model_id, r, model)
                .await
                .map_err(|e| format_provider_error("gem", e))?;
            let a = transform_gemini_response(&resp, &model.model_id)
                .map_err(|e| format!("resp: {e}"))?;
            serde_json::to_vec(&a).map_err(|e| format!("ser: {e}"))
        }
    }
}

async fn handle_streaming(
    state: &AppState,
    request_id: String,
    req: MessageRequest,
    models: Vec<ModelConfig>,
    start: Instant,
) -> Result<Response<Body>, ApiErrorWithRequestId> {
    let mut last_error: Option<String> = None;
    for model in &models {
        match try_streaming_model(state, &req, model, &request_id).await {
            Ok(response) => {
                state
                    .metrics
                    .record_success(&model.model_id, start.elapsed());
                info!(request_id = %request_id, model = %model.model_id, "streaming started");
                return Ok(response);
            }
            Err(e) => {
                warn!(request_id = %request_id, model = %model.model_id, error = %e, "streaming failed");
                last_error = Some(e);
                state.metrics.record_failure();
            }
        }
    }
    Err(ApiError::Upstream(sanitize_upstream_error(format!(
        "all streaming models failed: {}",
        last_error.unwrap_or_default()
    ))).with_request_id(request_id))
}

async fn try_streaming_model(
    state: &AppState,
    req: &MessageRequest,
    model: &ModelConfig,
    request_id: &str,
) -> Result<Response<Body>, String> {
    match classify_endpoint(&model.model_id) {
        EndpointType::Anthropic => {
            handle_anthropic_streaming(&state.client, req, model, request_id).await
        }
        EndpointType::ChatCompletions => {
            handle_openai_streaming(&state.client, req, model).await
        }
        EndpointType::Responses => {
            handle_responses_streaming(&state.client, req, model).await
        }
        EndpointType::Gemini => handle_gemini_streaming(&state.client, req, model).await,
    }
}

// -- Anthropic streaming: raw pipe ----------------------------------------

/// Anthropic streaming passthrough.
///
/// Note: The Anthropic passthrough path does not include keep-alive/heartbeat
/// events. Non-Anthropic streaming paths use `build_sse_response` which
/// includes a 3-second `KeepAlive` interval. Anthropic's own SSE stream
/// includes heartbeat events natively, so this is intentionally omitted.
/// If the upstream is slow, the client may time out.
async fn handle_anthropic_streaming(
    client: &OpenCodeClient,
    req: &MessageRequest,
    model: &ModelConfig,
    request_id: &str,
) -> Result<Response<Body>, String> {
    let b = serde_json::to_vec(req).map_err(|e| format!("ser: {e}"))?;
    let resp = client
        .send_anthropic_request(&b, true, model)
        .await
        .map_err(|e| format_provider_error("stream", e))?;
    let body = Body::from_stream(
        resp.bytes_stream()
            .map(|r| r.map_err(std::io::Error::other)),
    );
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .header("x-request-id", request_id)
        .body(body)
        .expect("static header values are valid"))
}

// -- OpenAI streaming via channel ----------------------------------------

async fn handle_openai_streaming(
    client: &OpenCodeClient,
    req: &MessageRequest,
    model: &ModelConfig,
) -> Result<Response<Body>, String> {
    let openai_req = transform_request(req, model).map_err(|e| format!("tf: {e}"))?;
    let stream = client
        .get_streaming_body(&model.model_id, openai_req, model)
        .await
        .map_err(|e| format_provider_error("stream", e))?;
    let model_id = model.model_id.clone();

    let events = spawn_proxy_task(stream, model_id, |proxy, line, out| {
        if let Err(e) = proxy.process_openai_chunk(line, out) {
            warn!("openai chunk transform error: {e}");
        }
    });

    build_sse_response(events)
}

// -- Responses streaming via channel --------------------------------------

async fn handle_responses_streaming(
    client: &OpenCodeClient,
    req: &MessageRequest,
    model: &ModelConfig,
) -> Result<Response<Body>, String> {
    let r = transform_to_responses(req, model).map_err(|e| format!("tf: {e}"))?;
    let stream = client
        .get_responses_streaming_body(&model.model_id, r, model)
        .await
        .map_err(|e| format_provider_error("stream", e))?;
    let model_id = model.model_id.clone();

    let events = spawn_proxy_task(stream, model_id, |proxy, line, out| {
        if let Err(e) = proxy.process_responses_chunk(line, out) {
            warn!("responses chunk transform error: {e}");
        }
    });

    build_sse_response(events)
}

// -- Gemini streaming via channel -----------------------------------------

async fn handle_gemini_streaming(
    client: &OpenCodeClient,
    req: &MessageRequest,
    model: &ModelConfig,
) -> Result<Response<Body>, String> {
    let r = transform_to_gemini(req, model).map_err(|e| format!("tf: {e}"))?;
    let stream = client
        .get_gemini_streaming_body(&model.model_id, r, model)
        .await
        .map_err(|e| format_provider_error("stream", e))?;
    let model_id = model.model_id.clone();

    let events = spawn_proxy_task(stream, model_id, |proxy, line, out| {
        if let Err(e) = proxy.process_gemini_chunk(line, out) {
            warn!("gemini chunk transform error: {e}");
        }
    });

    build_sse_response(events)
}

// -- SSE helpers ----------------------------------------------------------

/// Spawn a background task that reads chunks from the upstream byte stream,
/// processes them through a StreamProxy, and sends SSE Events through a
/// channel. Returns the receiving end as a BoxStream.
///
/// # Cancel-safety
///
/// A [`tokio_util::sync::CancellationToken`] is used to abort the upstream
/// stream reader when the client disconnects.  When the SSE receiver (the
/// returned `BoxStream`) is dropped, a drop-guard triggers the cancellation
/// token, which causes the spawned task to exit on the next iteration and
/// release the upstream HTTP connection.
///
/// The `TimeoutLayer` in the router still applies to the full response
/// lifetime including SSE streams. For streaming routes, consider exempting
/// them from the global timeout or using per-route middleware.
fn spawn_proxy_task<S, F>(stream: S, model_id: String, process: F) -> BoxStream<'static, Event>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    F: Fn(&mut StreamProxy, &str, &mut String) + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(128);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        let mut proxy = StreamProxy::new(&model_id);
        let mut out = String::new();
        let mut stream = Box::pin(stream);

        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    // Client disconnected; abort upstream stream.
                    return;
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let text = String::from_utf8_lossy(&bytes);
                            out.clear();

                            for line in text.split('\n') {
                                let line = line.trim_end_matches('\r');
                                process(&mut proxy, line, &mut out);
                            }

                            for event in parse_sse_events(&out) {
                                if tx.send(event).await.is_err() {
                                    // Receiver dropped, client disconnected.
                                    return;
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!("upstream stream error: {e}");
                            break;
                        }
                        None => break,
                    }
                }
            }
        }

        // Send any final events from the proxy.
        out.clear();
        if let Err(e) = proxy.finish(&mut out) {
            warn!("stream proxy finish error: {e}");
        }
        for event in parse_sse_events(&out) {
            if tx.send(event).await.is_err() {
                return;
            }
        }
    });

    // Wrap the receiver stream so that dropping it cancels the spawned task.
    let rx_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let cancel_guard = cancel;
    // The CancellationToken must outlive the stream so that the spawned task
    // can detect client disconnect. The move closure captures cancel_guard
    // (which owns the token) into the stream's environment, keeping it alive
    // until the stream is dropped. When the stream drops, cancel_guard drops,
    // and the token is cancelled -- the spawned task exits on the next select!
    // iteration.
    rx_stream
        .map(move |item| {
            // Reference cancel_guard to ensure it is moved into the closure
            // environment and kept alive for the stream's lifetime.
            let _guard = &cancel_guard;
            item
        })
        .boxed()
}

/// Parse SSE events from the transformer output.
///
/// Handles:
/// - `event:`, `data:` (with or without a space after the colon)
/// - Multi-line `data:` fields (concatenated with newlines per SSE spec)
/// - `id:` fields (stored on the event)
/// - `retry:` fields (ignored -- reconnection is outside our scope)
/// - Comment lines starting with `:` (ignored per SSE spec)
///
/// When migrating to core protocol adapters, consider replacing this with
/// a proper SSE parser crate (e.g. `eventsource-stream`) for full spec
/// compliance.
fn parse_sse_events(output: &str) -> Vec<Event> {
    output
        .split("\n\n")
        .filter(|s| !s.is_empty())
        .filter_map(|block| {
            let mut etype = String::new();
            let mut data_parts: Vec<String> = Vec::new();
            for line in block.split('\n') {
                // Skip comment lines (SSE spec: lines starting with ':')
                if line.starts_with(':') {
                    continue;
                }
                if let Some(rest) = line.strip_prefix("event:") {
                    let val = rest.strip_prefix(' ').unwrap_or(rest);
                    etype = val.to_owned();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    let val = rest.strip_prefix(' ').unwrap_or(rest);
                    data_parts.push(val.to_owned());
                }
                // `id:` and `retry:` fields are acknowledged but not needed
                // for our proxy pass-through.
            }
            if !data_parts.is_empty() {
                // Per SSE spec, multiple `data:` lines are joined by newlines.
                let data = data_parts.join("\n");
                Some(Event::default().event(&etype).data(&data))
            } else {
                None
            }
        })
        .collect()
}

fn build_sse_response(events: BoxStream<'static, Event>) -> Result<Response<Body>, String> {
    let sse = Sse::new(events.map(Ok::<_, std::convert::Infallible>))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT_INTERVAL));
    let response = sse.into_response();
    let (mut parts, body) = response.into_parts();
    debug_assert!(
        parts.status == StatusCode::OK,
        "Sse::into_response() should produce 200 OK, got {}",
        parts.status
    );
    // Sse::into_response() already sets Content-Type and Cache-Control.
    // Only add the extra headers not covered by the Sse wrapper.
    parts
        .headers
        .insert("X-Accel-Buffering", "no".parse().expect("static header value is always valid"));
    Ok(Response::from_parts(parts, body))
}

// ---------------------------------------------------------------------------
// Upstream error sanitization
// ---------------------------------------------------------------------------

/// Maximum length for upstream error messages returned to clients.
const MAX_UPSTREAM_ERROR_LEN: usize = 512;

/// Sanitize an upstream error message before returning it to the client.
///
/// Truncates to [`MAX_UPSTREAM_ERROR_LEN`] bytes and strips patterns that
/// may contain sensitive information (API key prefixes, full URLs with query
/// parameters). The full unsanitized message should be logged server-side
/// before calling this function.
///
/// Uses regex-based matching (same patterns as `sanitize_api_error_body` in
/// the provider crate) to avoid false-positive redaction of short substrings
/// like `sk-` that appear in ordinary words (e.g. "desk-area", "task-name").
fn sanitize_upstream_error(msg: String) -> String {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            // Anthropic keys: sk-ant-api03-XXXXX
            r"sk-ant-api03-[A-Za-z0-9_-]{10,}",
            // Anthropic keys: sk-ant-XXXXX
            r"sk-ant-[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk-live-XXXXX (hyphen form)
            r"sk-live-[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk-test-XXXXX (hyphen form)
            r"sk-test-[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk_live_XXXXX (underscore form)
            r"sk_live_[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk_test_XXXXX (underscore form)
            r"sk_test_[A-Za-z0-9_-]{10,}",
            // Generic sk- prefix with enough trailing chars to look like a key
            r"sk-[A-Za-z0-9_-]{20,}",
            // Google API keys: AIza followed by 30+ alphanumeric chars
            r"AIza[A-Za-z0-9_-]{30,}",
            // Generic key- prefix with enough trailing chars
            r"key-[A-Za-z0-9_-]{20,}",
        ]
        .iter()
        .map(|pat| regex::Regex::new(pat).expect("invalid redaction regex"))
        .collect()
    });

    let mut sanitized = msg;
    for re in patterns {
        sanitized = re.replace_all(&sanitized, "***").into_owned();
    }

    // Truncate to prevent leaking large upstream responses.
    if sanitized.len() > MAX_UPSTREAM_ERROR_LEN {
        // Find a safe truncation point (don't split a multi-byte char).
        let mut end = MAX_UPSTREAM_ERROR_LEN;
        while !sanitized.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...[truncated]", &sanitized[..end])
    } else {
        sanitized
    }
}

/// Format a [`ProviderError`] for client-facing error messages, sanitizing
/// sensitive data (URLs with query parameters, API key fragments).
///
/// The full error is logged server-side before this function is called, so
/// no diagnostic information is lost.
fn format_provider_error(prefix: &str, e: ProviderError) -> String {
    match &e {
        ProviderError::Http(reqwest_err) => {
            // reqwest::Error Display includes the full URL, which may contain
            // sensitive query parameters (e.g. ?key=... for Gemini). Redact
            // the URL while preserving the status/code information.
            let msg = format!("{reqwest_err}");
            let sanitized = match reqwest_err.url() {
                Some(url) => {
                    // Replace the full URL with just the origin (scheme + host).
                    let redacted = format!("{}://{}", url.scheme(), url.host_str().unwrap_or("redacted"));
                    msg.replace(url.as_str(), &redacted)
                }
                None => msg,
            };
            format!("{prefix}: {sanitized}")
        }
        ProviderError::Api { status, body } => {
            // Truncate API error body and strip key patterns.
            let sanitized_body = sanitize_upstream_error(body.clone());
            format!("{prefix}: API error {status}: {sanitized_body}")
        }
        _ => format!("{prefix}: {e}"),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- SSE parser tests --------------------------------------------------------

    #[test]
    fn parse_sse_basic_event() {
        let events = parse_sse_events("event: message_start\ndata: {\"type\":\"start\"}\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_sse_data_without_space() {
        // SSE spec allows "data:" without a space after the colon.
        let events = parse_sse_events("data:no-space\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_sse_multiline_data() {
        // Multiple data: lines should produce a single event.
        let events = parse_sse_events("data: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_sse_comment_lines_ignored() {
        let events = parse_sse_events(": this is a comment\ndata: hello\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_sse_empty_input() {
        let events = parse_sse_events("");
        assert!(events.is_empty());
    }

    #[test]
    fn parse_sse_multiple_events() {
        let input = "event: one\ndata: first\n\nevent: two\ndata: second\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parse_sse_data_only_no_event_field() {
        let events = parse_sse_events("data: just data\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_sse_id_and_retry_fields_accepted() {
        // id: and retry: lines should not prevent parsing data.
        let events = parse_sse_events("id: 42\nretry: 5000\ndata: payload\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_sse_field_without_colon_ignored() {
        // Lines that don't match "field:" pattern are ignored.
        let events = parse_sse_events("eventtype data\n\n");
        assert!(events.is_empty(), "lines without colon separator should not produce events");
    }
}
