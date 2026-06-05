//! Integration tests for ops endpoints and the `/v1/messages` proxy route.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use llm_proxy_core::{Config, FallbackHandler};
use llm_proxy_provider::OpenCodeClient;
use llm_proxy_server::{AppState, BuildInfo, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn state() -> AppState {
    let config = Config::default();
    AppState::new(
        config,
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        },
        OpenCodeClient::new(Arc::new(Config::default())),
        FallbackHandler::new(3, Duration::from_secs(30)),
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
/// and validate the request shape. Any failure must be an upstream or auth
/// error -- never a bad-JSON or request-shape error (400 with
/// `invalid_request_error` for missing `model` / `messages` is acceptable;
/// 400 with a JSON parse error is not).
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
    let resp_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    // Route must be registered (not 404).
    assert_ne!(status, StatusCode::NOT_FOUND, "route must be registered");

    // If the response is an error, it must NOT be a JSON parse error.
    // Valid errors are: upstream failure (502), rate limit (429), duplicate (409),
    // or internal routing errors (500). A 400 with invalid JSON would indicate
    // the request body was malformed, which it is not.
    if status == StatusCode::BAD_REQUEST {
        let error_type = resp_body["error"]["type"].as_str().unwrap_or("");
        assert_ne!(
            error_type,
            "invalid_request_error",
            "valid Anthropic JSON should not produce invalid_request_error"
        );
    }
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
