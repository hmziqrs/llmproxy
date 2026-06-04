# Initial Server Plan

A plan for the first runnable cut of `llm-proxy`. Goal: a workspace that
builds, a binary that starts, an axum server that serves ops routes and
an echo chat route, with the protocol/provider/storage crates scaffolded
but empty.

## Scope

In scope:

- All 7 workspace members scaffolded (5 as empty `lib.rs` + `Cargo.toml`
  stubs; 2 fully wired).
- `apps/llm-proxy` binary boots the server.
- `crates/llm-proxy-server` builds the axum `Router`, holds config,
  error type, `AppState`, ops routes, and an OpenAI-shaped echo chat
  route.
- `crates/llm-proxy-core` defines the crate-level error type and a
  minimal `Config` type.
- Graceful shutdown on SIGINT/SIGTERM.
- Tracing via `tracing` + `tracing-subscriber`.
- `just` recipes for `build`, `test`, `lint`, `fmt`, `run`.
- `rustfmt.toml` with `max_width = 100`.
- `Cargo.lock` committed (binary-led workspace).
- Clippy clean, no `unwrap` in production paths, unit + integration
  tests for handlers.
- Build metadata (target triple, git SHA) via `vergen-gix`, reported
  by `/version`.
- Config path resolved by `clap` (`--config` flag with
  `$LLM_PROXY_CONFIG` fallback).
- `rust-toolchain.toml` pinning the edition-2024 toolchain.

Out of scope (deferred):

- Any real protocol decode/encode beyond the echo shape.
- Any provider client.
- Storage, API, protocol, provider crates stay empty stubs.
- Auth, rate limiting, CORS, TLS.
- SSE streaming. Echo is non-streaming for v1.

## Repository state (assumed starting point)

```
.gitignore              # already excludes /target, /config.toml, /data, *.db, /ref/*/
Cargo.toml              # 11 lines, members listed, NO workspace.package, NO lints
README.md
justfile                # ref-only recipes; will be extended
docs/                   # protocol-mini, protocol-normalization, README
ref/                    # Python reference projects (not in workspace)
```

`apps/` and `crates/` **do not exist** yet. `Cargo.lock` does not
exist. There are zero `.rs` files in the repo.

Toolchain target: `rustc 1.85+` (edition 2024). Confirmed working
on 1.96.

## File tree after this plan

```
.
├── Cargo.toml                       # workspace + lints + workspace.dependencies
├── Cargo.lock                       # generated, committed
├── rust-toolchain.toml              # pins channel for edition 2024
├── rustfmt.toml                     # max_width = 100
├── config.toml.example              # documented config template
├── justfile                         # extended with build/test/lint/fmt/run
├── apps/
│   └── llm-proxy/
│       ├── Cargo.toml
│       ├── build.rs                 # vergen-gix build metadata
│       └── src/
│           └── main.rs              # binary entry
└── crates/
    ├── llm-proxy-core/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── config.rs            # Config struct + loader
    │       └── error.rs             # CoreError (thiserror)
    ├── llm-proxy-server/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs               # re-exports + build_router()
    │   │   ├── state.rs             # AppState, BuildInfo
    │   │   ├── error.rs             # ApiError + IntoResponse
    │   │   ├── routes/
    │   │   │   ├── mod.rs           # router() — composes routes + middleware
    │   │   │   ├── health.rs        # /health, /ready, /version
    │   │   │   └── chat.rs          # POST /v1/chat/completions (echo)
    │   │   └── shutdown.rs          # shutdown_signal()
    │   └── tests/
    │       └── chat_echo.rs         # integration test
    ├── llm-proxy-protocol/          # empty stub
    ├── llm-proxy-provider/          # empty stub
    ├── llm-proxy-storage/           # empty stub
    └── llm-proxy-api/               # empty stub
```

## Empty stub crate convention

All four stub crates follow the same shape:

`Cargo.toml`:

```toml
[package]
name = "llm-proxy-<role>"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
llm-proxy-core = { workspace = true }
```

`src/lib.rs`:

```rust
//! llm-proxy-<role>: stub.
//!
//! Intentionally empty in v1. See `docs/protocol-normalization.md`
//! and `docs/protocol-mini.md` for what goes here.
```

`protocol`, `provider`, `storage` only depend on `core` if/when
they need to. `api` will eventually depend on `core`, `protocol`,
`provider`, `storage`; for the stub it depends on `core` only.

## Root `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = [
    "apps/llm-proxy",
    "crates/llm-proxy-core",
    "crates/llm-proxy-protocol",
    "crates/llm-proxy-provider",
    "crates/llm-proxy-storage",
    "crates/llm-proxy-api",
    "crates/llm-proxy-server",
]

[workspace.package]
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"

[workspace.dependencies]
# Internal
llm-proxy-core = { path = "crates/llm-proxy-core" }
llm-proxy-server = { path = "crates/llm-proxy-server" }

# Async / runtime
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal", "time"] }

# Web
axum = { version = "0.8", features = ["macros", "json"] }
# axum-serde 0.9 tracks axum 0.8 and ships the `Sonic<T>` extractor/
# responder behind the `sonic` feature (pulls sonic-rs).
axum-serde = { version = "0.9", features = ["sonic"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["trace", "timeout"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
humantime-serde = "1"

# CLI
clap = { version = "4", features = ["derive", "env"] }

# Errors
thiserror = "2"
anyhow = "1"

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

# Build metadata
vergen-gix = "1"

# Misc
uuid = { version = "1", features = ["v4"] }

[workspace.lints.rust]
future-incompatible = "warn"
nonstandard_style = "deny"
missing_debug_implementations = "warn"
missing_docs = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }

# `all` already covers the perf group (e.g. `large_enum_variant`), so it
# is not re-listed. Nursery lints (`redundant_clone`, `needless_collect`)
# are intentionally NOT enabled: they are allow-by-default precisely
# because they emit false positives — the same reason `pedantic` is
# omitted. Pedantic is deferred and can be re-enabled per-crate once each
# crate stabilizes.

[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"
```

Reference: rust-best-practices Ch. 2.5. `pedantic` deliberately
omitted; `needless_pass_by_value` and `needless_pass_by_ref`
deliberately omitted (they conflict with idiomatic axum extractor
code).

## `rustfmt.toml`

```toml
max_width = 100
edition = "2024"
```

## `rust-toolchain.toml`

Edition 2024 requires rustc ≥ 1.85. Pinning the channel makes the
plan's toolchain assumption enforceable rather than just documented, so
a contributor on an older Rust gets a clear rustup message instead of a
cryptic edition error.

```toml
[toolchain]
channel = "1.85"
components = ["rustfmt", "clippy"]
```

## `justfile` extensions

Append to the existing file. The `ref` recipes stay unchanged.

```just
[doc("Build the workspace")]
build *args:
    cargo build {{args}}

[doc("Run all tests")]
test *args:
    cargo test --workspace {{args}}

[doc("Lint the workspace; treats warnings as errors")]
lint:
    cargo clippy --all-targets --all-features --locked -- -D warnings

[doc("Format the workspace")]
fmt:
    cargo fmt --all

[doc("Run the server binary")]
run *args:
    cargo run -p llm-proxy -- {{args}}
```

Reference: rust-best-practices Ch. 2.1.

## `crates/llm-proxy-core`

Purpose: types and traits that don't belong to a specific layer. For v1
this is `Config` and the crate-level error.

### `Cargo.toml`

```toml
[package]
name = "llm-proxy-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
serde = { workspace = true }
thiserror = { workspace = true }
toml = { workspace = true }
humantime-serde = { workspace = true }
```

### `src/lib.rs`

```rust
#![deny(missing_docs)]

pub mod config;
pub mod error;

pub use config::Config;
pub use error::CoreError;
```

### `src/config.rs`

```rust
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Top-level server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind: SocketAddr,
    /// Maximum request duration before timeout. Accepts humantime
    /// strings in TOML, e.g. `request_timeout = "60s"`.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Public server name reported by `/version`.
    pub server_name: String,
}

impl Config {
    /// Load a config from a TOML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CoreError::ConfigLoad { path: path.to_path_buf(), source: e })?;
        toml::from_str(&raw).map_err(CoreError::ConfigParse)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".parse().expect("default socket addr is valid"),
            request_timeout: Duration::from_secs(60),
            server_name: env!("CARGO_PKG_NAME").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bind_is_localhost_or_all() {
        let cfg = Config::default();
        assert_eq!(cfg.bind.port(), 8080);
    }

    #[test]
    fn default_timeout_is_60_seconds() {
        let cfg = Config::default();
        assert_eq!(cfg.request_timeout, Duration::from_secs(60));
    }
}
```

### `src/error.rs`

```rust
use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur in `llm-proxy-core`.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Failed to read a config file from disk.
    #[error("failed to read config at {path}: {source}")]
    ConfigLoad {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Failed to parse a config file.
    #[error("failed to parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),
}
```

### `config.toml.example`

Committed at the repo root so the on-disk format is documented (the
`Duration` field is the one that bites people — `humantime-serde` makes
it a plain string). Every field mirrors a `Config::default()` value.

```toml
# Address the HTTP server binds to.
bind = "0.0.0.0:8080"
# Maximum request duration before timeout (humantime string).
request_timeout = "60s"
# Public server name reported by /version.
server_name = "llm-proxy"
```

## `crates/llm-proxy-server`

Purpose: the axum wiring. `build_router(state) -> Router` is the public
function. State is generic-style here only as far as the type system
demands; the binary injects the concrete `AppState`.

### `Cargo.toml`

```toml
[package]
name = "llm-proxy-server"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
llm-proxy-core = { workspace = true }

axum = { workspace = true }
axum-serde = { workspace = true }
tokio = { workspace = true }
tower = { workspace = true }
tower-http = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
# `ServiceExt::oneshot` in the integration test needs tower's `util`
# feature. Scoped to dev so the library build stays lean; cargo's
# feature unification enables it only when compiling tests.
tower = { workspace = true, features = ["util"] }
```

### `src/lib.rs`

```rust
#![deny(missing_docs)]

pub mod error;
pub mod routes;
pub mod shutdown;
pub mod state;

pub use error::ApiError;
pub use shutdown::shutdown_signal;
pub use state::{AppState, BuildInfo};

use axum::Router;

/// Build the application router for the given state.
#[must_use]
pub fn build_router(state: AppState) -> Router {
    routes::router(state)
}
```

### `src/state.rs`

```rust
use std::sync::Arc;

use llm_proxy_core::Config;

/// Application state shared with every handler.
///
/// Cheap to clone: `Config` is small and `Arc`-wrapped fields share
/// allocation. Required by axum's `State` extractor. `Debug` is
/// mandatory under the `missing_debug_implementations` lint.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Server configuration.
    pub config: Arc<Config>,
    /// Build identifier reported by `/version`.
    pub build: Arc<BuildInfo>,
}

/// Static information about this binary.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// Package name.
    pub name: &'static str,
    /// Package version.
    pub version: &'static str,
    /// Target triple the binary was compiled for.
    pub target: &'static str,
    /// Git commit SHA the binary was built from.
    pub git_sha: &'static str,
}

impl AppState {
    /// Construct a new `AppState` from config and build info.
    #[must_use]
    pub fn new(config: Config, build: BuildInfo) -> Self {
        Self { config: Arc::new(config), build: Arc::new(build) }
    }
}
```

Notes (Ch. 1 Borrowing & Ownership, Ch. 9 Send/Sync): wrap heavy
fields in `Arc`. Keep `Config` cloneable. State is `Clone`, never
`&mut` across handlers — use `Arc` for interior mutability later.

### `src/error.rs`

```rust
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

/// API-layer error type. Implements `IntoResponse` so handlers can
/// return `Result<T, ApiError>`.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Bad client input.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Anything else.
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorBodyInner<'a>,
}

#[derive(Serialize)]
struct ErrorBodyInner<'a> {
    message: String,
    kind: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let body = ErrorBody {
            error: ErrorBodyInner { message: self.to_string(), kind },
        };
        (status, Json(body)).into_response()
    }
}
```

Notes (Ch. 4): `thiserror` enum, no `unwrap`, no `panic`. The
`Internal` variant is logged at the call site, not here.

### `src/routes/mod.rs`

```rust
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

use crate::state::AppState;

mod chat;
mod health;

use chat::echo_chat;
use health::{health, ready, version};

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024; // 32 MiB

/// Build the full router.
///
/// Middleware order, from outermost to innermost:
/// 1. `TraceLayer`  — logs every request and response, measures latency.
/// 2. `TimeoutLayer` — cancels requests exceeding `config.request_timeout`.
/// 3. `DefaultBodyLimit` — caps the request body for JSON extractors.
///
/// `ServiceBuilder` makes the *first* `.layer()` the outermost, so the
/// layers are listed here in the same outer-to-inner order. `TraceLayer`
/// must be outermost for it to observe requests later layers reject
/// (timeouts, oversized bodies).
pub fn router(state: AppState) -> Router {
    let timeout = state.config.request_timeout;
    let middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::new(timeout))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/v1/chat/completions", post(echo_chat))
        .fallback(not_found)
        .layer(middleware)
        .with_state(state)
}

async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "not found")
}
```

Reference: axum-skill "Middleware Execution Order". With
`ServiceBuilder`, `.layer()` calls applied in this order produce
outer-to-inner execution (TraceLayer runs first on the way in,
last on the way out).

### `src/routes/health.rs`

```rust
use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
}

/// Liveness probe. Always 200 if the process is up.
pub async fn health() -> (StatusCode, Json<HealthBody>) {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
}

#[derive(Serialize)]
struct ReadyBody {
    status: &'static str,
}

/// Readiness probe. Returns 200. Will check downstream state in a
/// later phase.
pub async fn ready() -> (StatusCode, Json<ReadyBody>) {
    (StatusCode::OK, Json(ReadyBody { status: "ready" }))
}

#[derive(Serialize)]
struct VersionBody {
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
```

### `src/routes/chat.rs`

> **v1 limitations.** This is an echo handler. It does not consult
> any provider, does not implement the OpenAI spec beyond the
> response shape, and does not stream. Real protocol decoding
> lands in `llm-proxy-protocol` per `docs/protocol-mini.md` §4.
> Non-string user content is logged and coerced to its JSON form.

```rust
use axum::extract::State;
use axum_serde::Sonic;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
}

#[derive(Deserialize)]
struct ChatMessage {
    role: String,
    content: Value,
}

#[derive(Serialize)]
struct ChatResponse {
    id: String,
    object: &'static str,
    created: i64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Serialize)]
struct Choice {
    index: u32,
    message: ResponseMessage,
    finish_reason: &'static str,
}

#[derive(Serialize)]
struct ResponseMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// POST /v1/chat/completions — non-streaming echo.
///
/// Accepts any well-formed JSON, rejects `stream: true` with 400,
/// otherwise returns the last user message echoed back wrapped in
/// the OpenAI chat completion shape. Uses `Sonic` (sonic-rs, SIMD) on
/// both the request and response — this is the one route where payload
/// size justifies it; ops/error routes stay on serde_json's `Json`.
pub async fn echo_chat(
    State(_state): State<AppState>,
    Sonic(req): Sonic<ChatRequest>,
) -> Result<Sonic<ChatResponse>, ApiError> {
    if req.stream {
        return Err(ApiError::BadRequest(
            "streaming not supported in this build".to_string(),
        ));
    }
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .ok_or_else(|| ApiError::BadRequest("no user message in request".to_string()))?;
    let text = match &last_user.content {
        Value::String(s) => s.clone(),
        other => {
            warn!(?other, "non-string user content coerced to JSON");
            other.to_string()
        }
    };
    Ok(Sonic(ChatResponse {
        id: format!("chatcmpl-{}", Uuid::new_v4()),
        object: "chat.completion",
        created: now_unix(),
        model: req.model,
        choices: vec![Choice {
            index: 0,
            message: ResponseMessage { role: "assistant", content: text },
            finish_reason: "stop",
        }],
        usage: Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
    }))
}

/// Current Unix time in seconds. The `unwrap_or(0)` is a documented
/// exception to the no-`unwrap` rule (Ch. 4.2): `duration_since`
/// only errors if the wall clock is set before 1970, which we treat
/// as a benign `0` rather than panicking. (Note: `SystemTime` is the
/// wall clock and can move backwards — that is exactly why this
/// returns a `Result` we must handle.)
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

### `src/shutdown.rs`

```rust
use tokio::signal;

/// Wait for SIGINT (Ctrl-C) or SIGTERM (Unix only).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}
```

### `tests/chat_echo.rs`

```rust
use axum::{body::Body, http::{Request, StatusCode}};
use llm_proxy_core::Config;
use llm_proxy_server::{AppState, BuildInfo, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn state() -> AppState {
    AppState::new(
        Config::default(),
        BuildInfo { name: "test", version: "0.0.0", target: "test", git_sha: "test" },
    )
}

#[tokio::test]
async fn chat_echo_returns_last_user_message() {
    let app = build_router(state());
    let body = json!({
        "model": "gpt-4o",
        "messages": [
            { "role": "system", "content": "be terse" },
            { "role": "user",   "content": "hello" }
        ]
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap(),
    )
    .unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
}

#[tokio::test]
async fn chat_rejects_stream_true() {
    let app = build_router(state());
    let body = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": true
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn health_returns_ok() {
    let app = build_router(state());
    let req = Request::builder().uri("/health").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
```

## `apps/llm-proxy` binary

### `Cargo.toml`

```toml
[package]
name = "llm-proxy"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "llm-proxy"
path = "src/main.rs"

[lints]
workspace = true

[dependencies]
llm-proxy-server = { workspace = true }
llm-proxy-core = { workspace = true }

anyhow = { workspace = true }
clap = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

[build-dependencies]
vergen-gix = { workspace = true }
```

### `build.rs`

Replaces the broken `env!("TARGET")` (cargo never sets `TARGET` for the
crate being compiled — only for build scripts). `vergen-gix` emits the
target triple and git SHA as compile-time env vars that `main.rs` reads
with `env!`. This repo is a git checkout, so the SHA resolves; for a
source build without git metadata, omitting `.fail_on_error()` lets
`vergen` substitute an idempotent placeholder instead of failing.
Confirm the exact builder API against the pinned `vergen-gix` 1.x docs.

```rust
use vergen_gix::{CargoBuilder, Emitter, GixBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // VERGEN_CARGO_TARGET_TRIPLE
    let cargo = CargoBuilder::default().target_triple(true).build()?;
    // VERGEN_GIT_SHA (short)
    let gix = GixBuilder::default().sha(true).build()?;
    Emitter::default()
        .add_instructions(&cargo)?
        .add_instructions(&gix)?
        .emit()?;
    Ok(())
}
```

### `src/main.rs`

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use llm_proxy_core::Config;
use llm_proxy_server::{AppState, BuildInfo, build_router, shutdown_signal};
use tokio::net::TcpListener;
use tracing::info;

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(name = "llm-proxy", version, about)]
struct Cli {
    /// Path to a TOML config file. Falls back to `$LLM_PROXY_CONFIG`,
    /// then to built-in defaults.
    #[arg(long, env = "LLM_PROXY_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config = match cli.config {
        Some(p) => {
            Config::load(&p).with_context(|| format!("loading config from {}", p.display()))?
        }
        None => Config::default(),
    };

    let bind = config.bind;
    let state = AppState::new(config, build_info());
    let app = build_router(state);

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding to {bind}"))?;
    info!(addr = %listener.local_addr()?, "llm-proxy listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    info!("server stopped cleanly");
    Ok(())
}

fn build_info() -> BuildInfo {
    BuildInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        target: env!("VERGEN_CARGO_TARGET_TRIPLE"),
        git_sha: env!("VERGEN_GIT_SHA"),
    }
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::registry().with(filter).with(fmt::layer()).init();
}
```

Notes (Ch. 4.4): `anyhow::Result` is correct here because this is
the binary. `Config::load` returns `Result<_, CoreError>` for the
library caller; main wraps it with `.context()` for the user.

## Skill mapping

### From `axum-web-framework`

| Section | Use here |
|---|---|
| Router, `nest`, `with_state` | `routes::router()` |
| Extractors: `State`, `Json`, `Sonic` (sonic-rs) | `health`, `chat`, integration test |
| Custom error type + `IntoResponse` | `ApiError` |
| Middleware: `TraceLayer`, `TimeoutLayer`, `DefaultBodyLimit`, order via `ServiceBuilder` | `routes::router()` |
| Fallback | `not_found` |
| Graceful shutdown via `axum::serve(...).with_graceful_shutdown` | `main.rs` |
| Testing with `tower::ServiceExt::oneshot` | `tests/chat_echo.rs` |
| Health/readiness/version | `routes::health.rs` |

### From `rust-best-practices`

| Chapter | Application |
|---|---|
| Ch. 1 Borrowing & Ownership | `Arc<Config>`, `&str` over `String` in DTOs, no needless clones |
| Ch. 2 Linting | workspace lints, `just lint` recipe, `cargo clippy --all-targets --all-features --locked -- -D warnings` |
| Ch. 3 Performance | avoid `.collect()` in hot paths, profile later |
| Ch. 4 Error handling | `thiserror` in libs, `anyhow` in binary, no `unwrap` outside tests and the documented timestamp fallback |
| Ch. 5 Testing | one assertion per test, integration test per route |
| Ch. 6 Generics | `AppState` is concrete, no `dyn` yet |
| Ch. 7 Type State | not needed at this stage; revisit when config has hot-reload |
| Ch. 8 Docs | `///` on every public item, `#![deny(missing_docs)]` |
| Ch. 9 Send/Sync | `AppState: Clone`; future tasks: `Send + 'static` |

## Phases

Each phase ends with a green `cargo build` and the workspace in a
state the next phase can consume. `--locked` is used **only after**
`Cargo.lock` exists in the repo.

### Phase 0 — Workspace metadata and stubs

Order matters: create directories and stub manifests **first**, then
edit the root `Cargo.toml`. Doing the root edit first leaves the
workspace members pointing at non-existent paths and `cargo build`
fails.

1. Create the directory tree for all 7 members.
2. Drop in the empty `Cargo.toml` and `src/lib.rs` for the 4 stub
   crates (`protocol`, `provider`, `storage`, `api`).
3. Edit root `Cargo.toml`: add `[workspace.package]`,
   `[workspace.dependencies]`, `[workspace.lints.rust]`,
   `[workspace.lints.clippy]`, `[profile.release]`.
4. Add root `rust-toolchain.toml` and `config.toml.example`.
5. `cargo build --workspace` to generate `Cargo.lock`.
6. `git add Cargo.lock` and commit (binary-led workspace).
7. `cargo build --workspace --locked` to confirm the lock is
   consistent.

### Phase 1 — `core`

1. Replace `crates/llm-proxy-core/Cargo.toml` with the versioned
   manifest using workspace deps.
2. Add `config.rs` and `error.rs` as in the snippets above.
3. `cargo test -p llm-proxy-core` green.
4. `just lint` green.

### Phase 2 — `server` (state, error, routes, shutdown, test)

1. Replace `crates/llm-proxy-server/Cargo.toml`.
2. Add `state.rs`, `error.rs`, `shutdown.rs`.
3. Add `routes/health.rs`, `routes/chat.rs`, `routes/mod.rs`.
4. Add `tests/chat_echo.rs`.
5. `cargo test -p llm-proxy-server` green.

### Phase 3 — `apps/llm-proxy` binary

1. Add `apps/llm-proxy/Cargo.toml`, `build.rs`, and `src/main.rs`.
2. `just build` green (confirms `vergen-gix` emits the env vars
   `main.rs` reads).
3. `just run` boots; `Ctrl-C` exits cleanly.
4. `cargo run -p llm-proxy -- --help` shows the `--config` flag.

### Phase 4 — Verification (full gate)

Run in order. Each must pass.

```sh
# Format
just fmt
git diff --exit-code  # no changes after fmt

# Lint (locked, deny warnings)
just lint

# Tests
just test

# Build
just build
```

## Verification commands (manual smoke)

After `just run` in one shell:

```sh
curl -sS http://127.0.0.1:8080/health
curl -sS http://127.0.0.1:8080/ready
curl -sS http://127.0.0.1:8080/version
curl -sS -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
```

Expected `/health` body:

```json
{ "status": "ok" }
```

Expected `/version` body (example; `name` comes from `config.server_name`):

```json
{ "name": "llm-proxy", "version": "0.1.0", "target": "aarch64-apple-darwin", "git_sha": "5c82a41" }
```

Expected chat echo body (shape, not values):

```json
{
  "id": "chatcmpl-<uuid>",
  "object": "chat.completion",
  "created": 1717000000,
  "model": "gpt-4o",
  "choices": [
    { "index": 0, "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }
  ],
  "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
}
```

## Dependency choices (build-it vs. use-a-crate)

The bias is toward small, well-maintained crates for anything fiddly or
spec-shaped, and hand-rolling only where a crate would be heavier than
the std code it replaces. Decisions for v1:

| Concern | v1 decision | Rationale |
|---|---|---|
| Build metadata (target, git SHA) | **`vergen-gix`** | `env!("TARGET")` does not compile; a build script is required anyway, and `vergen` gives the SHA for free, enriching `/version`. |
| Config path / CLI parsing | **`clap`** (`derive`, `env`) | One attribute handles both `--config` and `$LLM_PROXY_CONFIG`, reconciling the README (flag) with the env-var approach. Hand-rolling arg parsing scales poorly. |
| `Duration` in TOML | **`humantime-serde`** | Raw serde `Duration` deserializes from `{ secs, nanos }` — a usability trap. `"60s"` strings are obvious and self-documenting. |
| Graceful shutdown | **hand-rolled** (`tokio::signal` + `select!`) | This *is* the idiomatic axum-documented pattern. `tokio-graceful-shutdown` adds subsystem orchestration that v1 does not need. |
| Unix timestamp for `created` | **hand-rolled** | A 5-line `SystemTime` calc. Pulling in `time`/`jiff`/`chrono` is not justified until a date dependency exists for another reason. |
| Error response envelope | **hand-rolled** (custom shape) | Must mirror OpenAI, not a generic problem-details crate. See open question 8 — the current `{message, kind}` shape should converge on OpenAI's `{message, type, code}`. |
| JSON on the hot path | **`sonic-rs` via `axum-serde` `Sonic<T>`** | SIMD parse *and* serialize, serde-compatible, near-drop-in: swap `Json`→`Sonic` on the chat route. `serde_json` is kept for the tiny ops/error bodies and the dynamic `content` `Value`. `simd-json` was rejected — it needs `&mut` buffers and a hand-rolled extractor and wins only on parse. |

### Ecosystem crate map (post-v1)

Forward-looking — none of these enter the v1 build. Mapped to the stub
crates so each lands where it belongs, and to the cross-cutting concerns
the README already names ("pool keys, log usage, fail over").

**`llm-proxy-provider` — upstream clients**

| Crate | Role |
|---|---|
| `reqwest` (`json`, `stream`, `rustls-tls`) | HTTP client to providers |
| `reqwest-eventsource` / `eventsource-stream` | consume upstream SSE streams (OpenAI/Anthropic) |
| `backon` (or `backoff`) | retry + exponential backoff for failover |
| `reqwest-middleware` + `reqwest-retry` + `reqwest-tracing` | outbound HTTP middleware stack: retry policy and tracing spans on provider calls (cleaner than wrapping `backon` by hand) |
| `failsafe` | circuit breaker — stop hammering a provider that is failing, trip to the next one |
| `async-trait` / `trait-variant` | `dyn Provider` dispatch (edition 2024 has async-fn-in-trait, but `dyn` still needs boxing) |

**`llm-proxy-protocol` — schema & normalization**

| Crate | Role |
|---|---|
| `async-openai-types` | OpenAI request/response/stream types — don't re-derive content-part arrays, tool calls, logprobs |
| `serde_with` | provider quirks (string-or-array `content`, default-on-null, skip-empty) per `docs/research/quirks` |
| `base64` | encode/decode multimodal content (data-URL images, audio blobs) |
| `garde` (or `validator`) | declarative request validation beyond what `serde` enforces |

**`llm-proxy-storage` — persistence (`.gitignore`'s `*.db` ⇒ SQLite)**

| Crate | Role |
|---|---|
| `sqlx` (`sqlite`, `runtime-tokio`, `rustls`) | async, compile-time-checked queries + migrations |
| `time` or `jiff` | timestamps on usage/request rows (`sqlx` integrates with `time`) |
| `rust_decimal` | precise token-cost / spend accounting — never float money |

**Cross-cutting (server / api)**

| Crate | Role |
|---|---|
| `secrecy` | wrap API keys in `SecretString`; no leak via `Debug`/logs — essential once keys are pooled |
| `subtle` | constant-time API-key comparison (timing-safe auth) |
| `governor` + `tower_governor` | per-key rate limiting as a tower layer |
| `moka` | concurrent response / model-metadata cache |
| `tower-http` (`compression`, `request-id`, `cors`, `sensitive-headers`) | already a dep — enable features as needed; `sensitive-headers` redacts `Authorization` from traces |
| `metrics` + `metrics-exporter-prometheus` | usage logging / `/metrics` endpoint |
| `tiktoken-rs` (or a `bpe`-based tokenizer) | token counting so `usage` isn't hardcoded to 0 |
| `arc-swap` / `dashmap` | hot-swap config + concurrent key-pool state |
| `figment` | layered config (defaults → file → env); replaces `Config::load` |
| `axum-extra` (`headers`) | `TypedHeader<Authorization<Bearer>>` for the deferred auth (open question 5) |
| `tokio-util` (`CancellationToken`) | propagate graceful shutdown to background workers (e.g. usage-log flush) |
| `notify` | watch the config file for hot-reload (pairs with the Ch. 7 type-state note) |
| `dotenvy` | load `.env` in dev — `.gitignore` already expects it |
| `blake3` / `xxhash-rust` | fast cache-key hashing for the response cache |
| `rand` | generate proxy-issued API keys |

**Observability & testing**

| Crate | Role |
|---|---|
| `tracing-opentelemetry` + `opentelemetry-otlp` | distributed tracing |
| `wiremock` | mock upstream providers in tests (no live API calls in CI) |
| `insta` | snapshot/golden tests — directly implements `protocol-normalization.md` §10 "Golden test rule" |
| `rstest` | parameterized tests |
| `proptest` | property tests for normalization invariants (complements the `insta` goldens) |
| `axum-test` (`TestServer`) | nicer than `oneshot` boilerplate |

**Streaming (SSE)**

| Crate | Role |
|---|---|
| `axum::response::sse::Sse` (built-in) | server-side SSE responses — no extra crate needed |
| `futures` / `tokio-stream` | stream combinators to adapt provider events → client events |
| `async-stream` | `yield`-style ergonomic stream construction for the adapter |

**Scaling / distributed state** (only once running more than one replica)

| Crate | Role |
|---|---|
| `fred` (or `redis` + `deadpool`) | shared key pool, distributed rate limiting, and cross-replica cache — `governor`/`moka` are in-process only |

**API surface & transport**

| Crate | Role |
|---|---|
| `utoipa` + `utoipa-swagger-ui` | publish the OpenAI-compatible OpenAPI schema + docs UI |
| `axum` (`multipart`) / `axum-extra` | multipart endpoints beyond chat (`/v1/audio/*`, `/v1/files`) |
| `axum-server` (rustls) | in-process TLS termination if not behind a TLS-terminating reverse proxy |

**Binary / throughput**

| Crate | Role |
|---|---|
| `mimalloc` or `tikv-jemallocator` | allocator swap for the alloc-heavy forward path |
| `color-eyre` | richer binary error reports (optional alternative to `anyhow`) |

Reference Rust proxies validating this stack: Traceloop Hub (sqlx + OTel)
and LLM Link.

## Open questions / next steps after this plan

1. Real `CoreRequest`/`CoreEvent` types in `llm-proxy-protocol` —
   copy from `docs/protocol-mini.md` §4–5. Strongly consider
   `async-openai-types` rather than re-deriving the OpenAI schema.
2. SSE streaming. axum-skill section on `axum::response::sse::Sse`
   + `futures::stream`. Replace the `stream: true` rejection with
   a real event stream adapter.
3. Provider client skeletons in `llm-proxy-provider` per the
   priority order in `docs/protocol-mini.md` §3.
4. CORS layer (`tower-http::cors::CorsLayer`) once browser clients
   land.
5. Per-route auth middleware (extractor over a placeholder
   `Authorization` header) before exposing anything beyond ops.
6. Re-enable `pedantic` per-crate once each crate stabilizes.
7. Layered config via `figment` (defaults → file → env) when
   per-field env overrides are wanted.
8. Error-envelope compatibility: the body extractors (`Sonic` on the
   chat route, `Json` elsewhere) reject malformed bodies with their own
   400 *before* `ApiError` runs, so bad JSON does not get the
   `{error: …}` shape. Add a wrapper extractor (or rejection handler)
   and converge the envelope on OpenAI's `{message, type, param, code}`.
