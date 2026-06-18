use axum::{
    Json,
    extract::State,
    http::StatusCode,
};
use serde::Serialize;

use crate::state::AppState;

/// Health check response body.
///
/// Deliberately trivial (`{status, service}`): `/health` is the unauthenticated
/// liveness probe hammered by load balancers, so it must not expose operational
/// telemetry (audit LOW-13). Aggregate counters live on the in-process
/// [`Metrics`](llm_proxy_core::Metrics) snapshot for observability tooling, not
/// on this public route; a future authenticated admin or Prometheus endpoint
/// could surface them without putting them on the liveness probe.
///
/// Note: `#[serde(deny_unknown_fields)]` is intentionally omitted because these
/// structs are outbound-only (`Serialize`, never `Deserialize` from external
/// input). The attribute has no runtime effect on Serialize-only types.
#[derive(Serialize)]
pub(crate) struct HealthBody {
    status: &'static str,
    service: String,
}

/// Liveness probe.
///
/// Returns a minimal `{status, service}` body. No operational metrics are
/// exposed: the endpoint is unauthenticated and serves as a liveness probe, so
/// it must not leak telemetry (audit LOW-13). Query parameters are ignored.
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthBody>) {
    (
        StatusCode::OK,
        Json(HealthBody {
            status: "ok",
            service: state.server_name().to_owned(),
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
