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
