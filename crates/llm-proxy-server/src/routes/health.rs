use axum::{Json, extract::{Query, State}, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Query parameters for `/health`.
///
/// `metrics=true` opts in to the aggregate operational counters. Without it the
/// health body omits metrics entirely: the endpoint is unauthenticated, so
/// operational telemetry is not exposed by default (LOW-13).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct HealthQuery {
    #[serde(default)]
    metrics: bool,
}

/// Health check response body.
///
/// `metrics` is omitted from the serialized body unless the caller opts in via
/// `?metrics=true`, so a bare unauthenticated `/health` probe reveals no
/// operational telemetry (LOW-13).
///
/// Note: `#[serde(deny_unknown_fields)]` is intentionally omitted because these
/// structs are outbound-only (`Serialize`, never `Deserialize` from external
/// input). The attribute has no runtime effect on Serialize-only types.
///
#[derive(Serialize)]
pub(crate) struct HealthBody {
    status: &'static str,
    service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<HealthMetrics>,
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
    client_cancelled: i64,
}

/// Liveness probe.
///
/// Returns a minimal `{status, service}` body by default. Aggregate
/// operational counters are included only when the caller opts in via
/// `?metrics=true`; the endpoint is unauthenticated, so telemetry is not
/// exposed to bare probes (LOW-13). Per-provider and per-model counters are
/// always omitted.
pub async fn health(
    State(state): State<AppState>,
    Query(query): Query<HealthQuery>,
) -> (StatusCode, Json<HealthBody>) {
    let metrics = if query.metrics {
        let snapshot = state.metrics.get_snapshot();
        Some(HealthMetrics {
            requests_received: snapshot.requests_received,
            requests_streamed: snapshot.requests_streamed,
            requests_success: snapshot.requests_success,
            requests_failed: snapshot.requests_failed,
            upstream_calls: snapshot.upstream_calls,
            rate_limited: snapshot.rate_limited,
            deduplicated: snapshot.deduplicated,
            client_cancelled: snapshot.client_cancelled,
        })
    } else {
        None
    };

    (
        StatusCode::OK,
        Json(HealthBody {
            status: "ok",
            service: state.server_name().to_owned(),
            metrics,
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
///
/// This is currently a tautology (always returns "ready") because the proxy
/// has no external dependencies to check at startup.
///
/// TODO(future): Add actual readiness checks such as verifying provider
/// catalog cache freshness, confirming at least one provider is configured,
/// or checking downstream connectivity.
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

/// Build metadata for ops/debugging. `name` comes from `config.server_name`
/// (via `state.server_name()`) so operators can distinguish deployments; the
/// rest is compile-time build info. `BuildInfo.name` (the Cargo package name)
/// is not used here because it is a static constant that does not vary between
/// deployments.
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
