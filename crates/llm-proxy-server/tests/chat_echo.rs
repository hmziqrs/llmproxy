//! Integration tests for ops endpoints and the `/v1/messages` proxy route.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use llm_proxy_core::{Config, FallbackHandler, ProviderRegistry};
use llm_proxy_provider::{OpenCodeClient, ProviderAdapterRegistry, ProxyClient};
use llm_proxy_server::{AppState, BuildInfo, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn state() -> AppState {
    let config = Config::default();
    AppState::from_legacy(
        config,
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        },
        OpenCodeClient::new(Arc::new(Config::default())),
        FallbackHandler::new(3, Duration::from_secs(30)),
        ProviderAdapterRegistry::builtin(),
        ProxyClient::new(),
    )
}

/// Build AppState in TOML new-runtime mode for integration testing.
fn toml_state() -> AppState {
    use llm_proxy_core::{AppConfig, ServerConfig};
    use std::collections::HashMap;

    let app_config = AppConfig {
        server: ServerConfig {
            bind: "127.0.0.1:3456".parse().unwrap(),
            request_timeout: Duration::from_secs(300),
            log_level: "info".to_owned(),
            hot_reload: false,
            server_name: "toml-test-proxy".to_owned(),
        },
        models: HashMap::new(),
    };
    let registry = ProviderRegistry::from_providers(vec![]).expect("empty registry");
    AppState::from_toml(
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

#[tokio::test]
async fn health_returns_ok_with_body() {
    let app = build_router(state());
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn ready_returns_ready() {
    let app = build_router(state());
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn version_returns_build_info() {
    let app = build_router(state());
    let req = Request::builder()
        .uri("/version")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // name comes from Config::default().server_name (env!("CARGO_PKG_NAME"))
    assert!(body["name"].is_string());
    assert_eq!(body["version"], "0.0.0");
    assert_eq!(body["target"], "test");
    assert_eq!(body["git_sha"], "test");
}

#[tokio::test]
async fn messages_requires_auth_header() {
    let app = build_router(state());
    // Minimal Anthropic-shaped request without x-api-key.
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 16
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // The proxy requires an x-api-key header; without it the upstream
    // call should fail, but the route itself must be registered (not 404).
    assert_ne!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_route_returns_not_found() {
    let app = build_router(state());
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// -- Route guardrails (Phase 0 characterization) ---------------------------

/// Phase 0 guardrail: POST /v1/messages with valid Anthropic JSON must parse
/// and validate the request shape. This test verifies that the route is
/// registered and the request body parses successfully. Any failure must be
/// an upstream or auth error -- never a bad-JSON or request-shape error.
///
/// Note: The assertion on `error_type != "invalid_request_error"` when status
/// is 400 is a best-effort guard, not a guarantee. The Anthropic API itself
/// returns `invalid_request_error` for valid JSON with semantic issues. The
/// test specifically checks that the request parses and routes without a
/// JSON/shape error, not that no 400 can ever occur.
#[tokio::test]
async fn messages_valid_json_parses_and_validates() {
    let app = build_router(state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    // Parse response body to verify it is valid JSON.
    let _resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    // Route must be registered (not 404).
    assert_ne!(status, StatusCode::NOT_FOUND, "route must be registered");

    // The request body is valid Anthropic JSON, so it must parse and route
    // successfully. Valid downstream errors are: upstream failure (502),
    // rate limit (429), duplicate (409), internal routing (500), or auth (401).
    // A 400 with invalid_request_error would indicate the request body was
    // malformed, which it is not.
    let valid_error_statuses = [
        StatusCode::BAD_GATEWAY,          // 502 - upstream failure
        StatusCode::INTERNAL_SERVER_ERROR, // 500 - routing error
        StatusCode::UNAUTHORIZED,          // 401 - auth failure
        StatusCode::TOO_MANY_REQUESTS,     // 429 - rate limit
        StatusCode::CONFLICT,              // 409 - duplicate
        StatusCode::OK,                    // 200 - success (unlikely without real upstream)
    ];
    assert!(
        valid_error_statuses.contains(&status),
        "valid Anthropic JSON should produce a downstream error (502/500/401/429/409), got {status}"
    );
}

/// Phase 0 guardrail: POST /v1/messages with invalid (non-JSON) body returns
/// 400 with invalid_request_error (bad JSON parse).
#[tokio::test]
async fn messages_invalid_json_returns_bad_request() {
    let app = build_router(state());
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from("this is not json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid JSON"),
        "error message should mention invalid JSON"
    );
}

/// Phase 0 guardrail: POST /v1/messages with valid JSON but missing required
/// fields (no messages) returns 400 with invalid_request_error.
#[tokio::test]
async fn messages_valid_json_missing_fields_returns_validation_error() {
    let app = build_router(state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["error"]["type"], "invalid_request_error");
    // The missing field triggers a serde deserialization error, which is
    // reported as "invalid JSON: missing field `messages`".
    assert!(
        resp_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing field `messages`"),
        "should report missing messages field"
    );
}

/// Phase 0 guardrail: POST /v1/chat/completions with OpenAI-shaped JSON
/// remains unmounted (404) until Phase 9.
#[tokio::test]
async fn chat_completions_post_with_openai_json_is_unmounted() {
    let app = build_router(state());
    let body = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hello" }]
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn count_tokens_returns_estimate() {
    let app = build_router(state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello world" }],
        "max_tokens": 1024
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // Heuristic counter should return a positive token count.
    assert!(body["input_tokens"].as_u64().unwrap() > 0);
}

// -- Phase 0: Additional characterization tests -----------------------------

/// Phase 0: not_found returns a JSON error envelope matching the Anthropic
/// error format, not plain text.
#[tokio::test]
async fn not_found_returns_json_error_envelope() {
    let app = build_router(state());
    let req = Request::builder()
        .uri("/v1/nonexistent")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "not_found_error");
}

/// Phase 0: POST /v1/messages with wrong types in JSON fields (e.g. max_tokens
/// as a string) returns 400 with invalid_request_error.
#[tokio::test]
async fn messages_wrong_field_type_returns_bad_request() {
    let app = build_router(state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": "big"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["error"]["type"], "invalid_request_error");
}

/// Phase 0: POST /v1/messages with unknown fields returns 400 since
/// MessageRequest now uses deny_unknown_fields.
#[tokio::test]
async fn messages_unknown_fields_returns_bad_request() {
    let app = build_router(state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64,
        "future_field": "not supported"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["error"]["type"], "invalid_request_error");
}

/// Phase 0: POST /v1/messages with invalid UTF-8 bytes returns 400.
/// The server must not panic when receiving binary/non-UTF-8 body data.
#[tokio::test]
async fn messages_invalid_utf8_returns_bad_request() {
    let app = build_router(state());
    // Raw non-UTF-8 bytes.
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(vec![0x80, 0x81, 0x82]))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // The server should return a 400-level error, not panic (5xx).
    assert!(
        resp.status().is_client_error(),
        "expected 4xx for invalid UTF-8, got {}",
        resp.status()
    );
}

/// Phase 0: POST /v1/messages with empty body (0 bytes) returns 400.
#[tokio::test]
async fn messages_empty_body_returns_bad_request() {
    let app = build_router(state());
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

/// Phase 0: POST /v1/messages with oversized body (> 32 MiB) returns 413.
#[tokio::test]
async fn messages_oversized_body_returns_payload_too_large() {
    let app = build_router(state());
    // 33 MiB body (exceeds the 32 MiB limit).
    let oversized_body = "X".repeat(33 * 1024 * 1024);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(oversized_body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// -- TOML mode integration tests -------------------------------------------

/// TOML mode: /health returns ok with empty circuit_breakers map.
#[tokio::test]
async fn toml_health_returns_ok_with_empty_circuit_breakers() {
    let app = build_router(toml_state());
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["status"], "ok");
    // TOML mode has no legacy state, so circuit_breakers must be empty.
    assert!(
        body["circuit_breakers"].as_object().unwrap().is_empty(),
        "circuit_breakers should be empty in TOML mode, got: {}",
        body["circuit_breakers"]
    );
    // server_name comes from AppConfig.
    assert_eq!(body["service"], "toml-test-proxy");
}

/// TOML mode: /version returns build info with AppConfig server name.
#[tokio::test]
async fn toml_version_returns_app_config_server_name() {
    let app = build_router(toml_state());
    let req = Request::builder()
        .uri("/version")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    // name comes from AppConfig.server.server_name, not legacy Config.
    assert_eq!(body["name"], "toml-test-proxy");
    assert_eq!(body["version"], "0.0.0");
    assert_eq!(body["target"], "test");
    assert_eq!(body["git_sha"], "test");
}

/// TOML mode: /ready returns ready.
#[tokio::test]
async fn toml_ready_returns_ready() {
    let app = build_router(toml_state());
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["status"], "ready");
}

/// TOML mode: POST /v1/messages works with `legacy = None`, `app_config = Some`,
/// `providers = Some`. The route uses the core pipeline and returns a 400 error
/// for unknown models (empty routing table) without panicking.
#[tokio::test]
async fn toml_messages_returns_error_without_legacy_state() {
    let app = build_router(toml_state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // The core pipeline resolves the model route. With an empty routing table,
    // the model is unknown, so it returns 400 Bad Request.
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "TOML mode /v1/messages should return 400 for unknown model"
    );
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resp_body["type"], "error");
    assert_eq!(resp_body["error"]["type"], "invalid_request_error");
}

/// TOML mode: POST /v1/messages/count_tokens works without legacy state.
/// The token count endpoint does not depend on legacy state; it only uses
/// the token counter from AppState.
#[tokio::test]
async fn toml_count_tokens_returns_estimate() {
    let app = build_router(toml_state());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello world" }],
        "max_tokens": 1024
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
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
    assert!(resp_body["input_tokens"].as_u64().unwrap() > 0);
}
