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
use llm_proxy_provider::{EndpointType, OpenCodeClient, classify_endpoint};
use tracing::{error, info, warn};

use crate::error::ApiError;
use crate::middleware::get_client_ip;
use crate::state::AppState;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, ApiError> {
    let start = Instant::now();
    let request_id = state.request_id_gen.next_id();

    let client_ip = get_client_ip(&headers, None);
    if !state.rate_limiter.is_allowed(&client_ip) {
        state.metrics.record_rate_limited();
        return Err(ApiError::RateLimited(format!(
            "rate limit exceeded for {client_ip}"
        )));
    }

    if state.request_dedup.is_duplicate(&body) {
        state.metrics.record_deduplicated();
        return Err(ApiError::Duplicate(
            "duplicate request, please retry".to_owned(),
        ));
    }

    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::BadRequest(format!("invalid JSON: {e}")))?;

    req.validate().map_err(ApiError::BadRequest)?;

    let is_streaming = req.stream.unwrap_or(false);
    state.metrics.record_request(is_streaming);

    info!(request_id = %request_id, model = %req.model, streaming = is_streaming, "processing request");

    let scenario = detect_scenario_from_request(&req, &state, is_streaming);
    let model = state
        .model_router
        .resolve(&scenario.to_string())
        .ok_or_else(|| {
            ApiError::Internal(format!("no model configured for scenario: {scenario}"))
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
        handle_streaming(state, request_id, req, fallback_chain, start).await
    } else {
        handle_non_streaming(state, request_id, req, fallback_chain, start).await
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
    state: AppState,
    request_id: String,
    req: MessageRequest,
    models: Vec<ModelConfig>,
    start: Instant,
) -> Result<Response<Body>, ApiError> {
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
                .unwrap())
        } else {
            metrics.record_failure();
            Err(ApiError::Internal("no response body".to_owned()))
        }
    } else {
        metrics.record_failure();
        error!(request_id = %request_id, error = ?result.error, attempted = result.attempted, "all models failed");
        Err(ApiError::Upstream(
            result
                .error
                .unwrap_or_else(|| "all models failed".to_owned()),
        ))
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
                .map_err(|e| format!("anth: {e}"))?;
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
                .map_err(|e| format!("chat: {e}"))?;
            let a = transform_response(&resp, &model.model_id).map_err(|e| format!("resp: {e}"))?;
            serde_json::to_vec(&a).map_err(|e| format!("ser: {e}"))
        }
        EndpointType::Responses => {
            let r = transform_to_responses(req, model).map_err(|e| format!("tf: {e}"))?;
            let resp = client
                .responses_completion_non_streaming(&model.model_id, r, model)
                .await
                .map_err(|e| format!("resp: {e}"))?;
            let a = transform_responses_response(&resp, &model.model_id)
                .map_err(|e| format!("resp: {e}"))?;
            serde_json::to_vec(&a).map_err(|e| format!("ser: {e}"))
        }
        EndpointType::Gemini => {
            let r = transform_to_gemini(req, model).map_err(|e| format!("tf: {e}"))?;
            let resp = client
                .gemini_completion_non_streaming(&model.model_id, r, model)
                .await
                .map_err(|e| format!("gem: {e}"))?;
            let a = transform_gemini_response(&resp, &model.model_id)
                .map_err(|e| format!("resp: {e}"))?;
            serde_json::to_vec(&a).map_err(|e| format!("ser: {e}"))
        }
    }
}

async fn handle_streaming(
    state: AppState,
    request_id: String,
    req: MessageRequest,
    models: Vec<ModelConfig>,
    start: Instant,
) -> Result<Response<Body>, ApiError> {
    let mut last_error: Option<String> = None;
    for model in &models {
        match try_streaming_model(&state, &req, model).await {
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
    Err(ApiError::Upstream(format!(
        "all streaming models failed: {}",
        last_error.unwrap_or_default()
    )))
}

async fn try_streaming_model(
    state: &AppState,
    req: &MessageRequest,
    model: &ModelConfig,
) -> Result<Response<Body>, String> {
    match classify_endpoint(&model.model_id) {
        EndpointType::Anthropic => handle_anthropic_streaming(&state.client, req, model).await,
        EndpointType::ChatCompletions => handle_openai_streaming(&state.client, req, model).await,
        EndpointType::Responses => handle_responses_streaming(&state.client, req, model).await,
        EndpointType::Gemini => handle_gemini_streaming(&state.client, req, model).await,
    }
}

// -- Anthropic streaming: raw pipe ----------------------------------------

async fn handle_anthropic_streaming(
    client: &OpenCodeClient,
    req: &MessageRequest,
    model: &ModelConfig,
) -> Result<Response<Body>, String> {
    let b = serde_json::to_vec(req).map_err(|e| format!("ser: {e}"))?;
    let resp = client
        .send_anthropic_request(&b, true, model)
        .await
        .map_err(|e| format!("stream: {e}"))?;
    let body = Body::from_stream(
        resp.bytes_stream()
            .map(|r| r.map_err(std::io::Error::other)),
    );
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap())
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
        .map_err(|e| format!("stream: {e}"))?;
    let model_id = model.model_id.clone();

    let events = spawn_proxy_task(stream, model_id, |proxy, line, out| {
        let _ = proxy.process_openai_chunk(line, out);
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
        .map_err(|e| format!("stream: {e}"))?;
    let model_id = model.model_id.clone();

    let events = spawn_proxy_task(stream, model_id, |proxy, line, out| {
        let _ = proxy.process_responses_chunk(line, out);
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
        .map_err(|e| format!("stream: {e}"))?;
    let model_id = model.model_id.clone();

    let events = spawn_proxy_task(stream, model_id, |proxy, line, out| {
        let _ = proxy.process_gemini_chunk(line, out);
    });

    build_sse_response(events)
}

// -- SSE helpers ----------------------------------------------------------

/// Spawn a background task that reads chunks from the upstream byte stream,
/// processes them through a StreamProxy, and sends SSE Events through a
/// channel. Returns the receiving end as a BoxStream.
fn spawn_proxy_task<S, F>(stream: S, model_id: String, process: F) -> BoxStream<'static, Event>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    F: Fn(&mut StreamProxy, &str, &mut String) + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(128);

    tokio::spawn(async move {
        let mut proxy = StreamProxy::new(&model_id);
        let mut out = String::new();
        let mut stream = Box::pin(stream);

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break,
            };

            let text = String::from_utf8_lossy(&bytes);
            out.clear();

            for line in text.split('\n') {
                process(&mut proxy, line, &mut out);
            }

            for event in parse_sse_events(&out) {
                if tx.send(event).await.is_err() {
                    // Receiver dropped, client disconnected.
                    return;
                }
            }
        }

        // Send any final events from the proxy.
        out.clear();
        let _ = proxy.finish(&mut out);
        for event in parse_sse_events(&out) {
            if tx.send(event).await.is_err() {
                return;
            }
        }
    });

    tokio_stream::wrappers::ReceiverStream::new(rx).boxed()
}

fn parse_sse_events(output: &str) -> Vec<Event> {
    output
        .split("\n\n")
        .filter(|s| !s.is_empty())
        .filter_map(|block| {
            let mut etype = String::new();
            let mut data = String::new();
            for line in block.split('\n') {
                if let Some(et) = line.strip_prefix("event: ") {
                    etype = et.to_owned();
                } else if let Some(d) = line.strip_prefix("data: ") {
                    data = d.to_owned();
                }
            }
            if !data.is_empty() {
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
    parts.status = StatusCode::OK;
    parts
        .headers
        .insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    parts
        .headers
        .insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    parts
        .headers
        .insert(header::CONNECTION, "keep-alive".parse().unwrap());
    parts
        .headers
        .insert("X-Accel-Buffering", "no".parse().unwrap());
    Ok(Response::from_parts(parts, body))
}
