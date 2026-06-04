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
├── rustfmt.toml                     # max_width = 100
├── justfile                         # extended with build/test/lint/fmt/run
├── apps/
│   └── llm-proxy/
│       ├── Cargo.toml
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
axum = { version = "0.7", features = ["macros", "json"] }
tower = "0.5"
tower-http = { version = "0.5", features = ["trace", "timeout"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Errors
thiserror = "2"
anyhow = "1"

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

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
redundant_clone = "warn"
needless_collect = "warn"
large_enum_variant = "warn"

# Pedantic is intentionally NOT enabled for v1. It produces too many
# false positives on day one. Re-enable per-crate once each crate
# stabilizes.

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
    /// Maximum request duration before timeout.
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
tokio = { workspace = true }
tower = { workspace = true }
tower-http = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }
```

### `src/lib.rs`

```rust
#![deny(missing_docs)]

pub mod error;
pub mod routes;
pub mod shutdown;
pub mod state;

pub use error::ApiError;
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
/// allocation. Required by axum's `State` extractor.
#[derive(Clone)]
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
use std::time::Duration;

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

pub(self) use chat::echo_chat;
pub(self) use health::{health, ready, version};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024; // 32 MiB

/// Build the full router.
///
/// Middleware order, from outermost to innermost:
/// 1. `TraceLayer`  — logs every request and response, measures latency.
/// 2. `TimeoutLayer` — cancels requests exceeding `REQUEST_TIMEOUT`.
/// 3. `DefaultBodyLimit` — caps the request body for JSON extractors.
pub fn router(state: AppState) -> Router {
    let middleware = ServiceBuilder::new()
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
        .layer(TraceLayer::new_for_http());

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
    name: &'static str,
    version: &'static str,
    target: &'static str,
}

/// Build metadata for ops/debugging.
pub async fn version(State(state): State<AppState>) -> Json<VersionBody> {
    Json(VersionBody {
        name: state.build.name,
        version: state.build.version,
        target: state.build.target,
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
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
/// the OpenAI chat completion shape.
pub async fn echo_chat(
    State(_state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
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
    Ok(Json(ChatResponse {
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
/// exception to the no-`unwrap` rule (Ch. 4.2): `SystemTime` is a
/// monotonic clock on the supported targets and the fallback path
/// is not exercised in practice.
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
        BuildInfo { name: "test", version: "0.0.0", target: "test" },
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
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

### `src/main.rs`

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};
use llm_proxy_core::Config;
use llm_proxy_server::{AppState, BuildInfo, build_router, shutdown_signal};
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config_path: Option<PathBuf> = std::env::var("LLM_PROXY_CONFIG")
        .ok()
        .map(PathBuf::from);
    let config = match config_path {
        Some(p) => Config::load(&p).with_context(|| format!("loading config from {p:?}"))?,
        None => Config::default(),
    };

    let state = AppState::new(config.clone(), build_info());
    let app = build_router(state);

    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding to {}", config.bind))?;
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
        target: env!("TARGET"),
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
| Extractors: `State`, `Json` | `health`, `chat`, integration test |
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
4. `cargo build --workspace` to generate `Cargo.lock`.
5. `git add Cargo.lock` and commit (binary-led workspace).
6. `cargo build --workspace --locked` to confirm the lock is
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

1. Add `apps/llm-proxy/Cargo.toml` and `src/main.rs`.
2. `just build` green.
3. `just run` boots; `Ctrl-C` exits cleanly.

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

Expected `/version` body (example):

```json
{ "name": "llm-proxy", "version": "0.1.0", "target": "aarch64-apple-darwin" }
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

## Open questions / next steps after this plan

1. Real `CoreRequest`/`CoreEvent` types in `llm-proxy-protocol` —
   copy from `docs/protocol-mini.md` §4–5.
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
