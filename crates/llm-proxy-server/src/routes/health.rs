use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;
use std::collections::HashMap;

use crate::state::AppState;

/// Health check response body.
#[derive(Serialize)]
pub(crate) struct HealthBody {
    status: &'static str,
    service: String,
    metrics: HealthMetrics,
    circuit_breakers: HashMap<String, String>,
    model_counts: HashMap<String, i64>,
}

/// Metrics snapshot included in the health response.
#[derive(Serialize)]
struct HealthMetrics {
    requests_received: i64,
    requests_streamed: i64,
    requests_success: i64,
    requests_failed: i64,
    upstream_calls: i64,
    rate_limited: i64,
    deduplicated: i64,
}

/// Liveness probe with expanded metrics.
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthBody>) {
    let snapshot = state.metrics.get_snapshot();
    let circuit_breakers = state.fallback_handler.get_circuit_states();

    (
        StatusCode::OK,
        Json(HealthBody {
            status: "ok",
            service: state.config.server_name.clone(),
            metrics: HealthMetrics {
                requests_received: snapshot.requests_received,
                requests_streamed: snapshot.requests_streamed,
                requests_success: snapshot.requests_success,
                requests_failed: snapshot.requests_failed,
                upstream_calls: snapshot.upstream_calls,
                rate_limited: snapshot.rate_limited,
                deduplicated: snapshot.deduplicated,
            },
            circuit_breakers,
            model_counts: snapshot.model_counts,
        }),
    )
}

/// Readiness response body.
#[derive(Serialize)]
pub(crate) struct ReadyBody {
    status: &'static str,
}

/// Readiness probe. Returns 200. Will check downstream state in a
/// later phase.
pub async fn ready() -> (StatusCode, Json<ReadyBody>) {
    (StatusCode::OK, Json(ReadyBody { status: "ready" }))
}

/// Version response body.
#[derive(Serialize)]
pub(crate) struct VersionBody {
    /// Public server name, from `config.server_name`.
    name: String,
    version: &'static str,
    target: &'static str,
    git_sha: &'static str,
}

/// Build metadata for ops/debugging. `name` comes from config so
/// operators can distinguish deployments; the rest is compile-time
/// build info.
pub async fn version(State(state): State<AppState>) -> Json<VersionBody> {
    Json(VersionBody {
        name: state.config.server_name.clone(),
        version: state.build.version,
        target: state.build.target,
        git_sha: state.build.git_sha,
    })
}
