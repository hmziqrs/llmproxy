use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::state::AppState;

/// Health check response body.
///
/// Note: `#[serde(deny_unknown_fields)]` is intentionally omitted because these
/// structs are outbound-only (`Serialize`, never `Deserialize` from external
/// input). The attribute has no runtime effect on Serialize-only types.
///
#[derive(Serialize)]
pub(crate) struct HealthBody {
    status: &'static str,
    service: String,
    metrics: HealthMetrics,
}

/// Metrics snapshot included in the health response.
///
/// Note: `#[serde(deny_unknown_fields)]` is intentionally omitted (Serialize-only type).
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

/// Liveness probe with aggregate metrics.
///
/// Per-provider and per-model counters are intentionally omitted because this
/// endpoint is unauthenticated.
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthBody>) {
    let snapshot = state.metrics.get_snapshot();

    (
        StatusCode::OK,
        Json(HealthBody {
            status: "ok",
            service: state.server_name().to_owned(),
            metrics: HealthMetrics {
                requests_received: snapshot.requests_received,
                requests_streamed: snapshot.requests_streamed,
                requests_success: snapshot.requests_success,
                requests_failed: snapshot.requests_failed,
                upstream_calls: snapshot.upstream_calls,
                rate_limited: snapshot.rate_limited,
                deduplicated: snapshot.deduplicated,
            },
        }),
    )
}

/// Readiness response body.
///
/// Note: `#[serde(deny_unknown_fields)]` is intentionally omitted (Serialize-only type).
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
///
/// Note: `#[serde(deny_unknown_fields)]` is intentionally omitted (Serialize-only type).
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
pub async fn version(State(state): State<AppState>) -> (StatusCode, Json<VersionBody>) {
    let build = state.build_info();
    (
        StatusCode::OK,
        Json(VersionBody {
            name: state.server_name().to_owned(),
            version: build.version,
            target: build.target,
            git_sha: build.git_sha,
        }),
    )
}
