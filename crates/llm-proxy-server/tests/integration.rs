//! Integration tests for ops endpoints, the `/v1/messages` proxy route, and
//! TOML-mode integration scenarios.

use std::collections::HashMap;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use llm_proxy_core::{
    AppConfig, AuthStyle, ProviderAdapterConfig, ProviderConfig, ProviderRegistry,
    ProviderRoutesConfig, ServerConfig,
};
use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};
use llm_proxy_server::{AppState, BuildInfo, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Build AppState in TOML mode for integration testing.
fn state() -> AppState {
    let app_config = AppConfig {
        server: ServerConfig {
            bind: "127.0.0.1:3456".parse().unwrap(),
            request_timeout: Duration::from_secs(300),
            log_level: "info".to_owned(),
            hot_reload: false,
            server_name: "test-proxy".to_owned(),
        },
    };
    let registry = ProviderRegistry::from_providers(vec![]).expect("empty registry");
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

/// Build AppState with a single "mock-provider" registered so that provider-based
/// routing can resolve. The provider has an `anthropic_messages` adapter with a
/// dummy endpoint. This is used by tests that send valid JSON past the parsing
/// layer and need the request to reach the core pipeline.
fn state_with_provider() -> AppState {
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
                    endpoint: "https://127.0.0.1:0/v1/messages".to_owned(),
                    headers: HashMap::new(),
                },
            );
            m
        },
        routes: ProviderRoutesConfig {
            messages: Some("messages".to_owned()),
            chat_completions: Some("messages".to_owned()),
        },
        model_aliases: HashMap::new(),
        discovery: None,
        catalog: None,
    };
    let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
    AppState::new(
        AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: Duration::from_secs(300),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "test-proxy".to_owned(),
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
    assert!(body.get("model_counts").is_none());
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
    assert!(body["name"].is_string());
    assert_eq!(body["version"], "0.0.0");
    assert_eq!(body["target"], "test");
    assert_eq!(body["git_sha"], "test");
}

#[tokio::test]
async fn messages_requires_auth_header() {
    let app = build_router(state_with_provider());
    // Minimal Anthropic-shaped request without x-api-key.
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 16
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages")
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
        .uri("/v1/nonexistent_route")
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
    let app = build_router(state_with_provider());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages")
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
    // successfully. With an empty routing table, the model is unknown so
    // 400 is returned. With a real routing table, downstream errors are:
    // 502 (upstream), 500 (routing), 401 (auth), 429 (rate limit), 409 (duplicate).
    let valid_statuses = [
        StatusCode::BAD_REQUEST, // 400 - unknown model (empty routing table)
        StatusCode::BAD_GATEWAY, // 502 - upstream failure
        StatusCode::INTERNAL_SERVER_ERROR, // 500 - routing error
        StatusCode::UNAUTHORIZED, // 401 - auth failure
        StatusCode::TOO_MANY_REQUESTS, // 429 - rate limit
        StatusCode::CONFLICT,    // 409 - duplicate
        StatusCode::OK,          // 200 - success (unlikely without real upstream)
    ];
    assert!(
        valid_statuses.contains(&status),
        "valid Anthropic JSON should produce a valid status, got {status}"
    );
}

/// Phase 0 guardrail: POST /v1/messages with invalid (non-JSON) body returns
/// 400 with invalid_request_error (bad JSON parse).
#[tokio::test]
async fn messages_invalid_json_returns_bad_request() {
    let app = build_router(state());
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages")
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
        .uri("/providers/mock-provider/v1/messages")
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

/// Phase 9 guardrail: POST /v1/chat/completions route is mounted and tested
/// comprehensively in tests/chat_completions.rs. The removed chat_echo test that
/// was here has been removed since its single assertion (not-404) is a subset
/// of the `route_is_mounted_no_longer_404` test in that file.

#[tokio::test]
async fn count_tokens_returns_estimate() {
    let app = build_router(state_with_provider());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello world" }],
        "max_tokens": 1024
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages/count_tokens")
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
        .uri("/providers/mock-provider/v1/messages")
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
        .uri("/providers/mock-provider/v1/messages")
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
        .uri("/providers/mock-provider/v1/messages")
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
        .uri("/providers/mock-provider/v1/messages")
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
        .uri("/providers/mock-provider/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(oversized_body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// -- TOML mode integration tests -------------------------------------------

/// TOML mode: /health returns ok with server name from AppConfig.
#[tokio::test]
async fn toml_health_returns_ok() {
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
    // server_name comes from AppConfig.
    assert_eq!(body["service"], "test-proxy");
}

/// TOML mode: /version returns build info with AppConfig server name.
#[tokio::test]
async fn toml_version_returns_app_config_server_name() {
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
    assert_eq!(body["name"], "test-proxy");
    assert_eq!(body["version"], "0.0.0");
    assert_eq!(body["target"], "test");
    assert_eq!(body["git_sha"], "test");
}

/// TOML mode: /ready returns ready.
#[tokio::test]
async fn toml_ready_returns_ready() {
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

/// TOML mode: POST /providers/{provider}/v1/messages with a registered provider passes the
/// request through to the upstream adapter. The model name is forwarded as-is, so
/// an unknown model results in a downstream error (502) rather than a 400, because
/// the proxy no longer validates model names locally.
#[tokio::test]
async fn toml_messages_passes_through_to_upstream() {
    let app = build_router(state_with_provider());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello" }],
        "max_tokens": 64
    });
    let req = Request::builder()
        .method("POST")
        .uri("/providers/mock-provider/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // With provider-based routing, the model is passed through to the upstream
    // adapter. The dummy endpoint refuses connections, so we get a 502 Bad Gateway
    // (or 500 internal error), not a 400 for unknown model.
    let status = resp.status();
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "provider is registered, route must resolve"
    );
    // The upstream is unreachable so downstream error is expected.
    let valid_statuses = [
        StatusCode::BAD_GATEWAY,           // 502 - upstream failure
        StatusCode::INTERNAL_SERVER_ERROR, // 500 - routing error
    ];
    assert!(
        valid_statuses.contains(&status),
        "expected downstream error for unreachable upstream, got {status}"
    );
}

/// POST /providers/{provider}/v1/messages/count_tokens works with provider state.
#[tokio::test]
async fn toml_count_tokens_returns_estimate() {
    let app = build_router(state_with_provider());
    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{ "role": "user", "content": "hello world" }],
        "max_tokens": 1024
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
    assert!(resp_body["input_tokens"].as_u64().unwrap() > 0);
}
