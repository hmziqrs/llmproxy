use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::state::AppState;

/// Health check response body.
#[derive(Serialize)]
pub(crate) struct HealthBody {
    status: &'static str,
}

/// Liveness probe. Always 200 if the process is up.
pub async fn health() -> (StatusCode, Json<HealthBody>) {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
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
