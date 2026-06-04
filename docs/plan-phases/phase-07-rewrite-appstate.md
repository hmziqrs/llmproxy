# Phase 7 - Rewrite AppState

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: state carries the new config/registry/pipeline dependencies while old
routes can still compile until Phase 8.

### Files

Update:

```text
crates/llm-proxy-server/src/state.rs
crates/llm-proxy-server/src/routes/mod.rs
crates/llm-proxy-server/src/routes/health.rs
apps/llm-proxy/src/main.rs
```

### Final target state

```rust
#[derive(Clone, Debug)]
pub struct AppState {
    pub app_config: Arc<AppConfig>,
    pub providers: Arc<ProviderRegistry>,
    pub provider_adapters: Arc<ProviderAdapterRegistry>,
    pub proxy_client: Arc<ProxyClient>,
    pub build: Arc<BuildInfo>,
    pub token_counter: Arc<Counter>,
    pub metrics: Arc<Metrics>,
    pub rate_limiter: Arc<RateLimiter>,
    pub request_dedup: Arc<RequestDeduplicator>,
    pub request_id_gen: Arc<RequestIdGenerator>,
}
```

### Phase 7 transition state

During Phase 7 only, keep a single legacy bridge so all routes compile until
Phase 8/9 move to the core pipeline. Two standalone fields are not enough:
current `/v1/messages`, `/health`, `/version`, and router middleware read old
config, model router, fallback handler, and client state.

```rust
#[derive(Clone, Debug)]
pub struct LegacyState {
    pub config: Arc<Config>,
    pub client: Arc<OpenCodeClient>,
    pub model_router: Arc<ModelRouter>,
    pub fallback_handler: Arc<FallbackHandler>,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub app_config: Option<Arc<AppConfig>>,
    pub providers: Option<Arc<ProviderRegistry>>,
    pub provider_adapters: Arc<ProviderAdapterRegistry>,
    pub proxy_client: Arc<ProxyClient>,
    pub legacy: Option<Arc<LegacyState>>,
    pub build: Arc<BuildInfo>,
    pub token_counter: Arc<Counter>,
    pub metrics: Arc<Metrics>,
    pub rate_limiter: Arc<RateLimiter>,
    pub request_dedup: Arc<RequestDeduplicator>,
    pub request_id_gen: Arc<RequestIdGenerator>,
}
```

Add helper methods so route code does not scatter `legacy.as_ref()` checks:

```rust
impl AppState {
    pub fn request_timeout(&self) -> Duration;
    pub fn server_name(&self) -> &str;
    pub fn legacy(&self) -> Option<&LegacyState>;
    pub fn app_config(&self) -> Option<&AppConfig>;
    pub fn providers(&self) -> Option<&ProviderRegistry>;
}
```

Update `routes/mod.rs`, `/health`, and `/version` in this phase to use those
helpers. `/health` may return an empty `circuit_breakers` map when legacy state
is gone.

Add explicit constructors or invariants so mixed state is impossible:

```rust
impl AppState {
    pub fn from_legacy(...legacy fields...) -> Self;
    pub fn from_toml(...new runtime fields...) -> Self;
}
```

Do not expose or construct `OpenCodeClient`, `ModelRouter`, or
`FallbackHandler` in TOML new-runtime mode. They are allowed only inside
`LegacyState`.

Keep `LegacyState` for compatibility through Phase 10. Phase 11 owns final
deletion of `LegacyState` and other legacy server fields.

Valid Phase 7 construction modes:

- JSON compatibility mode: `legacy = Some(...)`, `app_config = None`,
  `providers = None`. Only old routes should use this mode.
- TOML new-runtime mode: `app_config = Some(...)`, `providers = Some(...)`.
  In this mode `legacy = None`. This is the mode Phase 8 and later route tests
  must use.
- Any mixed combination is invalid and must be rejected by constructors/tests.

Phase 8 is the point where live core-pipeline routes require TOML-backed
`app_config` and `providers`. Phase 10 formalizes the CLI migration behavior and
removes the last JSON-serving path.

### Main binary construction

In `cmd_serve`:

1. Resolve config path.
2. Build `ProviderAdapterRegistry::builtin()`.
3. Build `ProxyClient::new()`.
4. If path ends with `.toml`, load `AppConfig`, load providers from `providers/`
   next to the main config, pass adapter registry protocol names into core
   provider config validation, and build `AppState` in TOML new-runtime mode.
5. If path ends with `.json`, load old `Config` only during the Phase 7
   compatibility period and build `AppState` in JSON compatibility mode.
6. Any other extension fails config loading.

### Tests

Update integration test state construction in:

```text
crates/llm-proxy-server/tests/chat_echo.rs
```

The test state should not need a live provider API key. It should use a tiny
in-memory config/registry with a fake local endpoint where route tests need a
provider call, or avoid provider calls for pure ops route tests.

Add tests for:

- TOML `AppState` construction
- JSON legacy construction
- mixed construction rejected or impossible
- `.toml` provider protocol validation against builtin protocols
- `.json` compatibility construction during Phase 7 only
- unsupported config extensions fail
- `routes/mod.rs` and `/health` use helpers instead of direct legacy field
  access

### Gate

```sh
cargo test -p llm-proxy-server
cargo test --workspace
```
