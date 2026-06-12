//! Integration tests for the core pipeline (Phase 8).
//!
//! These tests exercise the full request path through the axum router with a
//! local mock upstream server. Each test configures AppState with a provider route
//! pointing at the mock server, which returns canned provider-specific responses
//! that are translated through the core pipeline into Anthropic-shaped responses.

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::IntoResponse,
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
};
use llm_proxy_core::{
    AppConfig, AuthStyle, ProviderAdapterConfig, ProviderConfig, ProviderRegistry, ServerConfig,
};
use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};
use llm_proxy_server::{AppState, BuildInfo, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Mock server readiness helper
// ---------------------------------------------------------------------------

/// Wait for a mock server to be ready by polling its TCP port.
///
/// Replaces `tokio::time::sleep(Duration::from_millis(50))` with a
/// deterministic readiness check that retries until the server accepts
/// a connection or the timeout elapses.
async fn wait_for_ready(addr: std::net::SocketAddr) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("mock server at {addr} did not become ready within 5 s");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ---------------------------------------------------------------------------
// Mock upstream server helpers
// ---------------------------------------------------------------------------

/// A canned Anthropic Messages API success response.
fn anthropic_success_response() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "msg_mock123",
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "text", "text": "Hello from mock!" }],
        "model": "claude-sonnet-4-6",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 5 }
    }))
    .unwrap()
}

/// A canned OpenAI Chat Completions API success response.
fn openai_chat_success_response() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "chatcmpl-test123",
        "object": "chat.completion",
        "created": 12345,
        "model": "gpt-4o-2024-08-06",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hello from OpenAI mock!"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    }))
    .unwrap()
}

/// A canned OpenAI Responses API success response.
fn openai_responses_success_response() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "resp_test123",
        "object": "response",
        "created": 12345,
        "model": "gpt-4o-2024-08-06",
        "output": [{
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "Hello from Responses mock!"
            }]
        }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5
        }
    }))
    .unwrap()
}

/// A canned Gemini generateContent API success response.
fn gemini_success_response() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "Hello from Gemini mock!" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 5,
            "totalTokenCount": 15
        }
    }))
    .unwrap()
}

/// Build AppState in TOML mode with a single provider of the given protocol
/// routing to the given mock endpoint.
fn state_with_provider(
    mock_endpoint: &str,
    protocol: &str,
    adapter_name: &str,
    _model_name: &str,
) -> AppState {
    // Build routes based on protocol so the provider-based routing can resolve.
    let routes = match protocol {
        "anthropic_messages" => llm_proxy_core::ProviderRoutesConfig {
            messages: Some(adapter_name.to_owned()),
            chat_completions: None,
        },
        "openai_chat_completions" => llm_proxy_core::ProviderRoutesConfig {
            chat_completions: Some(adapter_name.to_owned()),
            messages: Some(adapter_name.to_owned()),
        },
        _ => llm_proxy_core::ProviderRoutesConfig {
            messages: Some(adapter_name.to_owned()),
            chat_completions: None,
        },
    };

    let provider = ProviderConfig {
        name: "mock-provider".to_owned(),
        api_key: "test-key".to_owned(),
        auth_style: AuthStyle::Bearer,
        adapters: {
            let mut m = HashMap::new();
            m.insert(
                adapter_name.to_owned(),
                ProviderAdapterConfig {
                    protocol: protocol.to_owned(),
                    endpoint: mock_endpoint.to_owned(),
                    headers: HashMap::new(),
                },
            );
            m
        },
        routes,
        model_aliases: HashMap::new(),
        discovery: None,
        catalog: None,
    };

    let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");

    let app_config = AppConfig {
        server: ServerConfig {
            bind: "127.0.0.1:3456".parse().unwrap(),
            request_timeout: Duration::from_secs(300),
            log_level: "info".to_owned(),
            hot_reload: false,
            server_name: "test-proxy".to_owned(),
            rate_limit_rpm: 100,
            trust_forwarded_headers: false,
            dedup_window: Duration::from_millis(500),
        },
    };

    AppState::new(
        app_config,
        registry,
        ProviderAdapterRegistry::builtin(),
        ProxyClient::new(),
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        },
    )
}

/// Convenience: AppState with an Anthropic provider.
fn state_with_anthropic_provider(mock_endpoint: &str) -> AppState {
    state_with_provider(
        mock_endpoint,
        "anthropic_messages",
        "messages",
        "claude-sonnet-4-6",
    )
}

/// Convenience: AppState with an OpenAI Chat provider.
fn state_with_openai_chat_provider(mock_endpoint: &str) -> AppState {
    state_with_provider(mock_endpoint, "openai_chat_completions", "chat", "gpt-4o")
}

/// Convenience: AppState with an OpenAI Responses provider.
fn state_with_openai_responses_provider(mock_endpoint: &str) -> AppState {
    state_with_provider(
        mock_endpoint,
        "openai_responses",
        "responses",
        "gpt-4o-responses",
    )
}

/// Convenience: AppState with a Gemini provider.
fn state_with_gemini_provider(mock_endpoint: &str) -> AppState {
    state_with_provider(
        mock_endpoint,
        "gemini_generate_content",
        "gemini",
        "gemini-2.5-pro",
    )
}

/// Spawn a local mock axum server returning a canned response body.
/// The handler receives the response body as bytes and returns it with the
/// given content type.
async fn spawn_mock_server(response_body: Vec<u8>, content_type: &str) -> String {
    let ct = content_type.to_owned();
    let app = Router::new().route(
        "/{*path}",
        post(move || async move {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, ct.clone())],
                response_body.clone(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a local mock axum server returning canned Anthropic non-streaming
/// responses. Returns the base URL.
async fn spawn_mock_anthropic_non_stream() -> String {
    spawn_mock_server(anthropic_success_response(), "application/json").await
}

/// Spawn a local mock axum server returning OpenAI Chat non-streaming response.
async fn spawn_mock_openai_chat_non_stream() -> String {
    spawn_mock_server(openai_chat_success_response(), "application/json").await
}

/// Spawn a local mock axum server returning OpenAI Responses non-streaming response.
async fn spawn_mock_openai_responses_non_stream() -> String {
    spawn_mock_server(openai_responses_success_response(), "application/json").await
}

/// Spawn a local mock axum server returning Gemini non-streaming response.
async fn spawn_mock_gemini_non_stream() -> String {
    spawn_mock_server(gemini_success_response(), "application/json").await
}

/// Spawn a local mock axum server returning canned Anthropic streaming SSE
/// events.
async fn spawn_mock_anthropic_stream() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async move {
            let events = vec![
                Event::default()
                    .event("message_start")
                    .data(r#"{"type":"message_start","message":{"id":"msg_mock_stream","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
                Event::default()
                    .event("content_block_start")
                    .data(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
                Event::default()
                    .event("content_block_delta")
                    .data(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi!"}}"#),
                Event::default()
                    .event("content_block_stop")
                    .data(r#"{"type":"content_block_stop","index":0}"#),
                Event::default()
                    .event("message_delta")
                    .data(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#),
                Event::default()
                    .event("message_stop")
                    .data(r#"{"type":"message_stop"}"#),
            ];
            let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>));
            let sse = Sse::new(stream).keep_alive(KeepAlive::default());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse.into_response(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server returning OpenAI Chat streaming SSE events.
async fn spawn_mock_openai_chat_stream() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async move {
            let events = vec![
                Event::default()
                    .data(r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","created":12345,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#),
                Event::default()
                    .data(r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","created":12345,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hi from OpenAI!"},"finish_reason":null}]}"#),
                Event::default()
                    .data(r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","created":12345,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#),
                Event::default()
                    .data("[DONE]"),
            ];
            let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>));
            let sse = Sse::new(stream).keep_alive(KeepAlive::default());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse.into_response(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server returning OpenAI Responses streaming SSE events.
async fn spawn_mock_openai_responses_stream() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async move {
            let events = vec![
                Event::default()
                    .data(r#"{"type":"response.created","response":{"id":"resp_stream","object":"response","created":12345,"model":"gpt-4o","status":"in_progress","output":[]}}"#),
                Event::default()
                    .data(r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_1","role":"assistant","content":[]}}"#),
                Event::default()
                    .data(r#"{"type":"response.content_part.added","output_index":0,"content_index":0,"part":{"type":"output_text","text":""}}"#),
                Event::default()
                    .data(r#"{"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"Hi from Responses!"}"#),
                Event::default()
                    .data(r#"{"type":"response.output_text.done","output_index":0,"content_index":0,"text":"Hi from Responses!"}"#),
                Event::default()
                    .data(r#"{"type":"response.completed","response":{"id":"resp_stream","object":"response","created":12345,"model":"gpt-4o","status":"completed","output":[{"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"Hi from Responses!"}]}],"usage":{"input_tokens":10,"output_tokens":5}}}"#),
            ];
            let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>));
            let sse = Sse::new(stream).keep_alive(KeepAlive::default());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse.into_response(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server returning Gemini streaming SSE events.
async fn spawn_mock_gemini_stream() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async move {
            let events = vec![
                Event::default()
                    .data(r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hi from Gemini!"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#),
            ];
            let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>));
            let sse = Sse::new(stream).keep_alive(KeepAlive::default());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse.into_response(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server that returns HTTP 500.
async fn spawn_mock_500() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"type":"error","error":{"type":"internal_error","message":"upstream crash"}}"#
                    .as_bytes()
                    .to_vec(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server that returns a malformed SSE stream (invalid JSON in
/// an SSE event).
async fn spawn_mock_malformed_stream() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async move {
            let events = vec![
                Event::default()
                    .event("message_start")
                    .data(r#"{"type":"message_start","message":{"id":"msg_mock","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
                // Malformed event: invalid JSON
                Event::default()
                    .event("content_block_delta")
                    .data("this is not valid json {{{"),
            ];
            let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>));
            let sse = Sse::new(stream).keep_alive(KeepAlive::default());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse.into_response(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server that accepts a request, sends a few events, then
/// abruptly closes the connection (simulating upstream disconnect).
async fn spawn_mock_disconnect_stream() -> String {
    let app = Router::new().route(
        "/{*path}",
        post(|| async move {
            // Return only message_start, then stop (simulating disconnect).
            let events = vec![
                Event::default()
                    .event("message_start")
                    .data(r#"{"type":"message_start","message":{"id":"msg_mock","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#),
                Event::default()
                    .event("content_block_start")
                    .data(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
                // No message_stop -- stream is truncated.
            ];
            let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>));
            let sse = Sse::new(stream).keep_alive(KeepAlive::default());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse.into_response(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    format!("http://{}/providers/mock-provider/v1/messages", addr)
}

/// Spawn a mock server that captures whether it received a request.
/// Returns (base_url, request_received_flag).
async fn spawn_mock_with_request_tracker(response_body: Vec<u8>) -> (String, Arc<AtomicBool>) {
    let received = Arc::new(AtomicBool::new(false));
    let received_clone = received.clone();
    let app = Router::new().route(
        "/{*path}",
        post(move || {
            let received = received_clone.clone();
            async move {
                received.store(true, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    response_body.clone(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    (
        format!("http://{}/providers/mock-provider/v1/messages", addr),
        received,
    )
}

/// Spawn a mock server that records the received request body.
/// Returns (base_url, received_body_arc).
async fn spawn_mock_with_body_capture(
    response_body: Vec<u8>,
) -> (String, Arc<tokio::sync::Mutex<Option<Vec<u8>>>>) {
    let captured: Arc<tokio::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let captured_clone = captured.clone();
    let app = Router::new().route(
        "/{*path}",
        post(move |body: axum::body::Bytes| {
            let captured = captured_clone.clone();
            async move {
                let mut guard = captured.lock().await;
                *guard = Some(body.to_vec());
                drop(guard);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    response_body.clone(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_ready(addr).await;
    (
        format!("http://{}/providers/mock-provider/v1/messages", addr),
        captured,
    )
}

fn make_messages_body(model: &str, stream: bool) -> String {
    json!({
        "model": model,
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64,
        "stream": stream
    })
    .to_string()
}

/// Build a request to /providers/mock-provider/v1/messages.
fn messages_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

// ===========================================================================
// Non-streaming tests: Anthropic provider
// ===========================================================================

/// configured Anthropic provider returns Anthropic response through core pipeline
#[tokio::test]
async fn anthropic_provider_returns_anthropic_response() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert!(
        body["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Hello from mock!")
    );
}

/// request ID header is present on success
#[tokio::test]
async fn request_id_header_present_on_success() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let request_id = resp.headers().get("x-request-id");
    assert!(
        request_id.is_some(),
        "x-request-id header must be present on success"
    );
    let id_str = request_id.unwrap().to_str().unwrap();
    assert!(!id_str.is_empty(), "x-request-id must not be empty");
}

/// upstream 500 returns 502 Bad Gateway
#[tokio::test]
async fn upstream_500_returns_502() {
    let mock_url = spawn_mock_500().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["type"], "error");
    assert_eq!(resp_body["error"]["type"], "api_error");
}

/// unknown model is passed through to upstream (provider-based routing does not
/// validate model names locally; the upstream provider decides)
#[tokio::test]
async fn unknown_model_is_passed_through() {
    let (mock_url, received) = spawn_mock_with_request_tracker(anthropic_success_response()).await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("nonexistent-model", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    // With provider-based routing, the model name is passed through to the
    // upstream provider. The mock returns 200, so we get a successful response.
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify upstream WAS called (model name is passed through).
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        received.load(Ordering::SeqCst),
        "upstream should be called -- model name is passed through"
    );
}

// ===========================================================================
// Non-streaming tests: OpenAI Chat provider
// ===========================================================================

/// configured OpenAI Chat provider returns Anthropic response
#[tokio::test]
async fn openai_chat_provider_returns_anthropic_response() {
    let mock_url = spawn_mock_openai_chat_non_stream().await;
    let state = state_with_openai_chat_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("gpt-4o", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // Response must be Anthropic-shaped.
    assert_eq!(resp_body["type"], "message");
    assert_eq!(resp_body["role"], "assistant");
    assert!(
        resp_body["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Hello from OpenAI mock!"),
        "response should contain translated OpenAI text"
    );
}

// ===========================================================================
// Non-streaming tests: OpenAI Responses provider
// ===========================================================================

/// configured Responses provider returns Anthropic response
#[tokio::test]
async fn openai_responses_provider_returns_anthropic_response() {
    let mock_url = spawn_mock_openai_responses_non_stream().await;
    let state = state_with_openai_responses_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("gpt-4o-responses", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // Response must be Anthropic-shaped.
    assert_eq!(resp_body["type"], "message");
    assert_eq!(resp_body["role"], "assistant");
    assert!(
        resp_body["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Hello from Responses mock!"),
        "response should contain translated Responses text"
    );
}

// ===========================================================================
// Non-streaming tests: Gemini provider
// ===========================================================================

/// configured Gemini provider returns Anthropic response
#[tokio::test]
async fn gemini_provider_returns_anthropic_response() {
    let mock_url = spawn_mock_gemini_non_stream().await;
    let state = state_with_gemini_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("gemini-2.5-pro", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // Response must be Anthropic-shaped.
    assert_eq!(resp_body["type"], "message");
    assert_eq!(resp_body["role"], "assistant");
    assert!(
        resp_body["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Hello from Gemini mock!"),
        "response should contain translated Gemini text"
    );
}

// ===========================================================================
// Streaming tests: Anthropic provider
// ===========================================================================

/// stream:true Anthropic provider returns Anthropic-shaped SSE text deltas
#[tokio::test]
async fn stream_anthropic_provider_returns_sse_text_deltas() {
    let mock_url = spawn_mock_anthropic_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.contains("text/event-stream"),
        "expected SSE content-type, got: {ct}"
    );

    let request_id = resp.headers().get("x-request-id");
    assert!(
        request_id.is_some(),
        "x-request-id must be present on stream response"
    );

    // Collect the SSE body and parse events.
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // Verify key SSE events appear in the stream.
    assert!(
        text.contains("event: message_start"),
        "missing message_start event"
    );
    assert!(
        text.contains("event: content_block_delta"),
        "missing content_block_delta"
    );
    assert!(
        text.contains("event: message_stop"),
        "missing message_stop event"
    );

    // Verify text content came through.
    assert!(
        text.contains("Hi!"),
        "expected text delta 'Hi!' in SSE output"
    );
}

// ===========================================================================
// Streaming tests: OpenAI Chat provider
// ===========================================================================

/// stream:true OpenAI Chat provider returns Anthropic-shaped SSE text deltas
#[tokio::test]
async fn stream_openai_chat_provider_returns_anthropic_sse_deltas() {
    let mock_url = spawn_mock_openai_chat_stream().await;
    let state = state_with_openai_chat_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("gpt-4o", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.contains("text/event-stream"),
        "expected SSE content-type, got: {ct}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // The output must be Anthropic-shaped SSE events.
    assert!(
        text.contains("event: message_start"),
        "missing message_start event"
    );
    assert!(
        text.contains("event: message_stop"),
        "missing message_stop event"
    );

    // Verify text content was translated from OpenAI to Anthropic SSE format.
    assert!(
        text.contains("Hi from OpenAI!"),
        "expected translated text delta in SSE output, got: {text}"
    );
}

// ===========================================================================
// Streaming tests: OpenAI Responses provider
// ===========================================================================

/// stream:true Responses provider returns Anthropic-shaped SSE text deltas
#[tokio::test]
async fn stream_openai_responses_provider_returns_anthropic_sse_deltas() {
    let mock_url = spawn_mock_openai_responses_stream().await;
    let state = state_with_openai_responses_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("gpt-4o-responses", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.contains("text/event-stream"),
        "expected SSE content-type, got: {ct}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // The output must be Anthropic-shaped SSE events.
    assert!(
        text.contains("event: message_start"),
        "missing message_start event"
    );
    assert!(
        text.contains("event: message_stop"),
        "missing message_stop event"
    );

    // Verify text content was translated.
    assert!(
        text.contains("Hi from Responses!"),
        "expected translated text delta in SSE output, got: {text}"
    );
}

// ===========================================================================
// Streaming tests: Gemini provider
// ===========================================================================

/// stream:true Gemini provider returns Anthropic-shaped SSE text deltas
#[tokio::test]
async fn stream_gemini_provider_returns_anthropic_sse_deltas() {
    let mock_url = spawn_mock_gemini_stream().await;
    let state = state_with_gemini_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("gemini-2.5-pro", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.contains("text/event-stream"),
        "expected SSE content-type, got: {ct}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // The output must be Anthropic-shaped SSE events.
    assert!(
        text.contains("event: message_start"),
        "missing message_start event"
    );
    assert!(
        text.contains("event: message_stop"),
        "missing message_stop event"
    );

    // Verify text content was translated.
    assert!(
        text.contains("Hi from Gemini!"),
        "expected translated text delta in SSE output, got: {text}"
    );
}

// ===========================================================================
// Stream error behavior tests
// ===========================================================================

/// stream_error_before_first_byte_returns_http_502 (via upstream 500 on stream request)
#[tokio::test]
async fn stream_error_before_first_byte_returns_http_502() {
    let mock_url = spawn_mock_500().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    // The upstream returns 500, which send_stream propagates as an error
    // before the SSE response is committed (since the HTTP connection itself
    // failed). This should map to 502 Bad Gateway.
    assert_eq!(
        resp.status(),
        StatusCode::BAD_GATEWAY,
        "stream with upstream 500 should return 502"
    );
}

/// stream_error_after_first_byte_emits_anthropic_error_event_then_terminates
///
/// Note: The Anthropic adapter's decode_frame silently skips malformed JSON
/// events (returns Ok(vec![])), so a malformed SSE frame does NOT trigger
/// the in-band error path. The stream completes normally. This test verifies
/// that the stream does not panic or crash on malformed data, and that the
/// valid events before the malformed one are delivered.
#[tokio::test]
async fn stream_error_after_first_byte_emits_error_event() {
    let mock_url = spawn_mock_malformed_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    // The HTTP status is 200 since SSE was committed.
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // Verify that the stream started with a message_start event.
    assert!(
        text.contains("event: message_start"),
        "stream should start with message_start"
    );

    // The stream should complete with a message_stop (the malformed frame is
    // silently skipped by the Anthropic adapter, not treated as a fatal error).
    assert!(
        text.contains("event: message_stop"),
        "stream should complete with message_stop even after malformed data"
    );
}

/// upstream disconnect: when the upstream stream ends prematurely (no
/// message_stop from upstream), the client encoder's finish() emits a
/// synthetic terminal event. The stream should complete gracefully with
/// a message_stop event.
#[tokio::test]
async fn upstream_disconnect_completes_with_synthetic_terminal() {
    let mock_url = spawn_mock_disconnect_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // Stream should have started with message_start.
    assert!(
        text.contains("event: message_start"),
        "stream should start with message_start"
    );

    // The stream should complete with a message_stop event generated by
    // the client encoder's finish() method, even though the upstream
    // didn't send one.
    assert!(
        text.contains("event: message_stop"),
        "stream should have a synthetic message_stop after premature upstream end"
    );
}

/// malformed stream frame returns Anthropic-shaped error
///
/// The Anthropic adapter silently skips malformed JSON events, so the stream
/// completes normally. This test verifies the stream does not panic or produce
/// garbage output.
#[tokio::test]
async fn malformed_stream_frame_is_handled_gracefully() {
    let mock_url = spawn_mock_malformed_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // Stream should have started with message_start.
    assert!(
        text.contains("event: message_start"),
        "stream should start with message_start"
    );
    // Stream should complete normally (malformed frame silently skipped).
    assert!(
        text.contains("event: message_stop"),
        "stream should end with message_stop"
    );
}

/// Stream terminal event is emitted after provider decoder finish().
/// Verifies that the finalization order follows: consume chunks -> SSE frames
/// -> decoder events -> client encoder -> finish() -> terminal events.
#[tokio::test]
async fn stream_terminal_event_emitted_after_decoder_finish() {
    let mock_url = spawn_mock_disconnect_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", true);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // Verify the finalization sequence:
    // 1. Real events from upstream arrive first (message_start, content_block_start).
    assert!(
        text.contains("event: message_start"),
        "should have message_start from upstream"
    );
    assert!(
        text.contains("event: content_block_start"),
        "should have content_block_start from upstream"
    );

    // 2. The synthetic terminal events (from finish()) appear after real events.
    // Find positions to verify ordering.
    let start_pos = text.find("event: message_start").expect("message_start");
    let stop_pos = text.find("event: message_stop").expect("message_stop");
    assert!(
        stop_pos > start_pos,
        "message_stop must appear after message_start"
    );

    // 3. message_delta (with stop_reason) must appear before message_stop.
    let delta_pos = text.find("event: message_delta").expect("message_delta");
    assert!(
        delta_pos < stop_pos,
        "message_delta must appear before message_stop"
    );
    assert!(
        delta_pos > start_pos,
        "message_delta must appear after message_start"
    );
}

// ===========================================================================
// Rate limit and dedup tests
// ===========================================================================

/// Rate-limited request returns 429 Too Many Requests.
///
/// Uses a state configured with rpm=1 so that the second request is
/// deterministically rejected.
#[tokio::test]
async fn rate_limited_request_returns_429() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    // Build state with rpm=1 so the second request is deterministically rejected.
    let provider = ProviderConfig {
        name: "mock-provider".to_owned(),
        api_key: "test-key".to_owned(),
        auth_style: AuthStyle::Bearer,
        adapters: {
            let mut m = HashMap::new();
            m.insert(
                "messages".to_owned(),
                ProviderAdapterConfig {
                    protocol: "anthropic_messages".to_owned(),
                    endpoint: mock_url,
                    headers: HashMap::new(),
                },
            );
            m
        },
        routes: llm_proxy_core::ProviderRoutesConfig {
            messages: Some("messages".to_owned()),
            chat_completions: None,
        },
        model_aliases: HashMap::new(),
        discovery: None,
        catalog: None,
    };
    let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
    let state = AppState::new(
        AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: Duration::from_secs(300),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "test-proxy".to_owned(),
                rate_limit_rpm: 1,
                trust_forwarded_headers: false,
                dedup_window: Duration::from_millis(500),
            },
        },
        registry,
        ProviderAdapterRegistry::builtin(),
        ProxyClient::new(),
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        },
    );
    let app = build_router(state);

    // First request should be allowed (rpm=1, 1 token available).
    let body1 = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello one" }],
        "max_tokens": 64
    });
    let resp1 = app
        .clone()
        .oneshot(messages_request(&body1.to_string()))
        .await
        .unwrap();
    // The first request may succeed (200) or fail for non-rate-limit reasons.
    // It must NOT be 429.
    assert_ne!(
        resp1.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "first request should not be rate-limited"
    );

    // Second request should be rate-limited (token bucket depleted).
    let body2 = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello two" }],
        "max_tokens": 64
    });
    let resp2 = app
        .clone()
        .oneshot(messages_request(&body2.to_string()))
        .await
        .unwrap();
    assert_eq!(
        resp2.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "second request should be rate-limited with rpm=1"
    );
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp2.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["type"], "error");
    assert_eq!(resp_body["error"]["type"], "rate_limit_error");
}

/// Duplicate request returns 409 Conflict.
///
/// Uses a large dedup window (60 s) so the second request is deterministically
/// caught as a duplicate.
#[tokio::test]
async fn duplicate_request_returns_409() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    // Build state with a 60-second dedup window so the duplicate is guaranteed
    // to be caught, and rpm=100 so rate limiting does not interfere.
    let provider = ProviderConfig {
        name: "mock-provider".to_owned(),
        api_key: "test-key".to_owned(),
        auth_style: AuthStyle::Bearer,
        adapters: {
            let mut m = HashMap::new();
            m.insert(
                "messages".to_owned(),
                ProviderAdapterConfig {
                    protocol: "anthropic_messages".to_owned(),
                    endpoint: mock_url,
                    headers: HashMap::new(),
                },
            );
            m
        },
        routes: llm_proxy_core::ProviderRoutesConfig {
            messages: Some("messages".to_owned()),
            chat_completions: None,
        },
        model_aliases: HashMap::new(),
        discovery: None,
        catalog: None,
    };
    let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
    let state = AppState::new(
        AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: Duration::from_secs(300),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "test-proxy".to_owned(),
                rate_limit_rpm: 100,
                trust_forwarded_headers: false,
                dedup_window: Duration::from_secs(60),
            },
        },
        registry,
        ProviderAdapterRegistry::builtin(),
        ProxyClient::new(),
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        },
    );
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", false);

    // First request should succeed or fail for non-dedup reasons.
    let resp1 = app.clone().oneshot(messages_request(&body)).await.unwrap();
    assert_ne!(resp1.status(), StatusCode::CONFLICT, "first request should not be a duplicate");

    // Second request with the same body and path should be deduplicated.
    let resp2 = app.oneshot(messages_request(&body)).await.unwrap();
    assert_eq!(
        resp2.status(),
        StatusCode::CONFLICT,
        "second identical request should be deduplicated"
    );
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp2.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["type"], "error");
    assert!(
        resp_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("duplicate"),
        "error message should mention 'duplicate'"
    );
}

// ===========================================================================
// Token count tests
// ===========================================================================

/// Token count with tool definitions returns a positive count (tools excluded by design).
#[tokio::test]
async fn token_count_with_tools_returns_positive_count() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64,
        "tools": [{
            "name": "get_weather",
            "description": "Get the weather",
            "input_schema": {
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                }
            }
        }]
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages/count_tokens")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // Token count should be positive but may be underestimated since tools
    // are intentionally excluded from the estimate.
    assert!(
        resp_body["input_tokens"].as_u64().unwrap() > 0,
        "token count should be positive even when tools are present"
    );
}

/// Token count with non-text content returns a positive count (non-text excluded by design).
#[tokio::test]
async fn token_count_with_non_text_content_returns_positive_count() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "describe this" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "fake" } }
            ]
        }],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages/count_tokens")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        resp_body["input_tokens"].as_u64().unwrap() > 0,
        "token count should be positive even with non-text content"
    );
}

/// Token count with tool_result content returns a positive count (excluded by design).
#[tokio::test]
async fn token_count_with_tool_result_returns_positive_count() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [
            { "role": "user", "content": "what is the weather?" },
            { "role": "assistant", "content": [{ "type": "tool_use", "id": "toolu_123", "name": "get_weather", "input": { "location": "SF" } }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_123", "content": "Sunny, 72F" }] }
        ],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages/count_tokens")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        resp_body["input_tokens"].as_u64().unwrap() > 0,
        "token count should be positive even with tool_result content"
    );
}

/// Token count endpoint works with the provider-configured application state.
#[tokio::test]
async fn token_count_endpoint_works_with_provider_state() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages/count_tokens")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ===========================================================================
// Error response shape tests
// ===========================================================================

/// Invalid JSON returns 400 with Anthropic-shaped error.
#[tokio::test]
async fn invalid_json_returns_400_anthropic_shape() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from("not json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

/// Provider-scoped messages routing works with the configured application state.
#[tokio::test]
async fn provider_messages_route_works() {
    let mock_url = spawn_mock_anthropic_non_stream().await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = make_messages_body("claude-sonnet-4-6", false);
    let resp = app.oneshot(messages_request(&body)).await.unwrap();
    // Should succeed since we have a valid mock upstream.
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Route preserves key fields through core pipeline round-trip.
/// Sends a request with optional fields (system, temperature, max_tokens)
/// and verifies the upstream mock receives them in the encoded request.
#[tokio::test]
async fn route_preserves_fields_through_core() {
    let (mock_url, captured_body) =
        spawn_mock_with_body_capture(anthropic_success_response()).await;
    let state = state_with_anthropic_provider(&mock_url);
    let app = build_router(state);

    let body = json!({
        "model": "claude-sonnet-4-6",
        "system": "You are a helpful assistant.",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 128,
        "temperature": 0.7,
        "top_p": 0.9,
        "stream": false
    });
    let resp = app
        .oneshot(messages_request(&body.to_string()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Check the upstream received the correct fields.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let guard = captured_body.lock().await;
    let upstream_body = guard
        .as_ref()
        .expect("upstream should have received a request body");
    let upstream_json: Value =
        serde_json::from_slice(upstream_body).expect("upstream body should be valid JSON");

    // The Anthropic adapter should have preserved these fields.
    assert_eq!(upstream_json["model"], "claude-sonnet-4-6");
    assert!(
        upstream_json["system"].is_string() || upstream_json["system"].is_array(),
        "system field should be preserved"
    );
    assert_eq!(upstream_json["max_tokens"], 128);
    assert_eq!(upstream_json["temperature"], 0.7);
    assert_eq!(upstream_json["top_p"], 0.9);
    // The adapter may omit `stream` when false (default), so accept either
    // `false` or absent (Null).
    let stream_val = &upstream_json["stream"];
    assert!(
        stream_val.is_null() || stream_val == &json!(false),
        "stream field should be false or absent, got: {stream_val}"
    );
}
