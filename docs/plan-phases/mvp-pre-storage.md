# MVP Pre-Storage: Token Counting, Upstream IDs, Pricing, Event Log, Secret Keys

## Goal

Ship the minimum viable set of features that give the storage and API
crates a real data model to persist and a safe public surface to expose.
None of these five features require storage or API work; together they
make the data flowing through the proxy real, attributable, and
non-leaking.

The five features, in the user's proposed order:

1. Real token counting via `tiktoken-rs`
2. Real upstream message IDs by buffering the first stream event
3. Cost/price config in TOML: `ProviderConfig.pricing: HashMap<ModelId, ModelPricing>`
4. Structured request/response event log: tracing JSON layer plus a dedicated `EventBus` trait
5. `secrecy::SecretString` for `api_key`, touching every config load

After this, storage has token counts, upstream IDs, cost, and structured
events to persist. API can expose usage/cost/routes without leaking keys.

## Scope

In scope:

- Replace the character-heuristic `Counter` with a real BPE tokenizer
  backed by `tiktoken-rs`, with model-to-encoding selection and a
  heuristic fallback for unknown models.
- Propagate the upstream provider's message ID to streaming clients by
  buffering the first `CoreEvent::MessageStart` before constructing the
  client stream encoder.
- Add a `ModelPricing` config type and a `pricing` map on
  `ProviderConfig`; compute per-request cost from `Usage` and pricing;
  surface cost on `CoreResponse` and the event log.
- Add a structured request/response event log: a JSON tracing layer for
  ops logs, and an `EventBus` trait plus `RequestEvent` / `ResponseEvent`
  types in the storage crate for the persistence sink.
- Wrap every `api_key: String` in `secrecy::SecretString`; remove the
  now-redundant manual `Debug` redaction impls; update all read sites and
  config validation.

Out of scope (deferred to storage/api phases):

- Persisting events to SQLite or any other backend.
- HTTP handlers in `llm-proxy-api` (the crate stays a stub).
- Native upstream token-count endpoints.
- Per-client cost quotas or spend limits.
- Hot-reload of pricing or keys.
- Retrofitting every `String` model field in the codebase to `ModelId`.

## Locked Decisions

1. **Token counting keeps the `Counter` API surface.** The `Counter` type
   stays as the public entry point; it gains a `Tokenizer` backend. The
   heuristic stays as the fallback for unknown models so the
   `/v1/messages/count_tokens` route never hard-fails on a new model id.
2. **`tiktoken-rs` is the tokenizer crate.** It bundles the BPE ranks for
   the OpenAI encodings (`cl100k_base`, `o200k_base`, `p50k_base`,
   `r50k_base`) at **compile time** via `include_bytes!` — no network
   fetch at runtime, offline-safe. `CoreBPE` is `Send + Sync`. No custom
   BPE. Lazy-loaded on first `count_tokens` for a known model prefix;
   cached in `Arc` for subsequent calls.
9. **`Counter::count_messages` signature changes to accept a model id.**
   This is the one call-site break in the plan (`token_count.rs:212`).
   The model id selects the BPE encoding; unknown models fall back to
   the heuristic. The old ZST `Counter` becomes a struct holding a
   `HashMap<String, Arc<dyn Tokenizer>>` + heuristic fallback.
3. **Streaming upstream IDs use a buffered-first-event approach, not a
   pre-flight request.** The first decoded `CoreEvent` is held until its
   type is known; if it is `MessageStart` with `Some(id)`, that id seeds
   the client encoder. Otherwise the existing synthetic id is used. No
   extra upstream round trip.
4. **`ModelId` is a minimal transparent newtype, not a wholesale
   retrofit.** It is introduced only on the new `pricing` map to match the
   user's specified shape `HashMap<ModelId, ModelPricing>`. Existing
   `String` model fields are not migrated in this plan.
5. **Money is `rust_decimal::Decimal`, never floating point.** Prices are
   stored per-token in USD as `Decimal`. Cost computation uses `Decimal`
   arithmetic and is rounded to the nearest microcent on output only.
6. **The event log has two sinks, staged.** Phase A adds a JSON
   `tracing-subscriber` layer for structured ops logs. Phase B adds an
   `EventBus` trait and `RequestEvent` / `ResponseEvent` types in
   `llm-proxy-storage` for the persistence sink. Both ship in this plan.
7. **`secrecy` is the secrets crate.** `SecretString` wraps `api_key` at
   all config layers (`ProviderConfig`, `ProviderAdapterTargetConfig`,
   `ProviderAdapterTarget`, `AuthHeaders`). Reads go through
   `expose_secret()` only at the two transport boundaries: `transport.rs`
   (`apply_auth`) and `discovery.rs` (direct auth-header construction).
   Manual `Debug` redaction impls are removed where `SecretString`'s
   built-in redaction covers them.
8. **Pricing is provider-scoped, not global.** Each provider's TOML
   declares prices for the model ids it serves. Aliases are resolved
   before pricing lookup so the upstream model id is the pricing key.

## Current State (anchors)

All file:line references are against the tree as of June 18, 2026.

### Token counting (heuristic only)

- `crates/llm-proxy-core/src/token/counter.rs:42` — `Counter` ZST,
  `CHARS_PER_TOKEN = 4` (`counter.rs:34`), `count_tokens` (`:55`),
  `count_messages` (`:81`). Module doc (`token/mod.rs:3-5`) states it is
  not tiktoken-compatible.
- `crates/llm-proxy-protocol/src/core.rs:831` — `Usage` struct with
  `input_tokens`, `output_tokens`, `reasoning_tokens`,
  `cache_creation_input_tokens`, `cache_read_input_tokens`,
  `provenance: UsageProvenance`.
- `crates/llm-proxy-server/src/routes/token_count.rs:45` — `count_tokens`
  handler; `count_tokens_inner` at `:86`. Registered at
  `routes/mod.rs:130-131`. Excludes tools/images at
  `token_count.rs:135-144`; response doc (`:23-30`) calls it an
  approximation.
- `crates/llm-proxy-server/src/state.rs:70` — `AppState.token_counter:
  Counter` inline ZST.

### Upstream message IDs (streaming gap)

- `crates/llm-proxy-protocol/src/core.rs:905` — `CoreEvent::MessageStart
  { id: Option<String>, model: ModelRef }`.
- `crates/llm-proxy-provider/src/adapter/anthropic.rs:298-308` — adapter
  populates `id` from the upstream `message_start` event.
- `crates/llm-proxy-server/src/routes/core_pipeline.rs:671-676` —
  synthetic `msg_id` generated and passed to `ClientStreamEncoder::new`.
  Explicit `TODO(v2)` at `core_pipeline.rs:667`.
- `crates/llm-proxy-protocol/src/client/anthropic.rs:622` and
  `client/openai_chat.rs:620` — `StreamEncoder::new(msg_id, model, ...)`
  already accept the id; the route just does not pass the upstream one.
- Non-streaming `CoreResponse.id: Option<String>` (`core.rs:749`) already
  echoes the upstream id; only streaming synthesizes.

### Pricing/cost (does not exist)

- No `pricing`, `cost`, `price`, or `ModelPricing` anywhere in `.rs`.
- Natural attachment points: `StaticModelCatalogEntry`
  (`provider_config.rs:445`) and `Usage` (`core.rs:831`).
- `validate_provider_config` at `provider_config.rs:789` is where new
  pricing validation hooks in.

### Event log (fmt layer only)

- `apps/llm-proxy/src/state.rs:129-139` — `init_tracing` with
  `fmt::layer()` only; `tracing-subscriber` declared with
  `["env-filter", "fmt"]` (`Cargo.toml:50`) — no `json` feature.
- Per-request span at `routes/mod.rs:257` carries `request_id`, method,
  path (path only, never the full URI, to avoid leaking query-string
  keys).
- `crates/llm-proxy-storage/src/lib.rs` is a 34-line stub; no `EventBus`,
  no event types.

### Secrets (manual redaction only)

- `crates/llm-proxy-core/src/provider_config.rs:149` —
  `ProviderConfig.api_key: String` with manual `Debug` redaction at
  `:169-182` and `#[serde(skip_serializing)]`.
- `crates/llm-proxy-core/src/provider_registry.rs:93` —
  `ProviderAdapterTargetConfig.api_key: String` with manual `Debug` at
  `:107-121`.
- `crates/llm-proxy-provider/src/adapter/mod.rs:112` —
  `ProviderAdapterTarget.api_key: String` with manual `Debug` at
  `:126-140`.
- `crates/llm-proxy-provider/src/transport.rs:40` —
  `AuthHeaders.api_key: String` (the auth-header carrier struct).
- Validation reads `provider.api_key` at `provider_config.rs:811,817`.
- Discovery reads `provider.api_key` directly at `discovery.rs:182-187`,
  bypassing `AuthHeaders` entirely.
- No `secrecy`, `SecretString`, `expose_secret`, or `Zeroize` anywhere.

## Implementation Order

The user's order (1-5) is a reasonable spine, but features 3 and 5 both
edit the same config files. Doing secrecy first avoids editing
`provider_config.rs` twice and lands the secret-handling discipline that
pricing config should be built on. Feature 4 depends conceptually on 1,
2, and 3 because the event log captures token counts, upstream ids, and
cost, so it should land last to capture all of them.

Recommended order (minimizes rework, respects dependencies):

```text
Step 0  F5  secrecy::SecretString         (foundational, touches config)
Step 1  F2  upstream message IDs          (independent, unblocks event log)
Step 2  F1  real token counting           (independent)
Step 3  F3  pricing config + cost         (touches config, after secrecy)
Step 4  F4  structured event log          (captures F1/F2/F3 data; lands last)
```

The user's original order (1-5) is also viable; the only real constraint
is that F4 should come after F1/F2/F3 so the event types can be authored
once against the final data shape. F2 is the smallest independent unit
and can safely go first in any ordering.

Each step ends with `cargo test --workspace`, `cargo clippy -- -D
warnings`, and `cargo fmt --all -- --check` green, and the workspace in a
state the next step can consume.

## Step 0 — secrecy::SecretString for api_key

### Why first

`secrecy::SecretString` redacts in `Debug` and `Display` by default and
zeroizes on drop. Adopting it first means the pricing config added in
Step 3 is built on a secret-safe foundation, and `provider_config.rs` is
edited once instead of twice.

### Dependency

Add to `[workspace.dependencies]` in the root `Cargo.toml`:

```toml
secrecy = { version = "0.10", features = ["serde", "alloc"] }
```

The `serde` feature gives `SecretString` `Serialize`/`Deserialize`
(needed for `ProviderConfig`'s serde derives). The `alloc` feature pulls
in `Zeroizing<String>` backing. Add `secrecy = { workspace = true }` to
`llm-proxy-core` and `llm-proxy-provider`.

### Type changes

`crates/llm-proxy-core/src/provider_config.rs`:

```rust
use secrecy::SecretString;

pub struct ProviderConfig {
    pub name: String,
    /// API key, wrapped to redact from Debug/Display/logs and zeroize on drop.
    /// `${ENV_VAR}` interpolation happens before this is constructed.
    #[serde(skip_serializing)]
    pub api_key: SecretString,
    // ...unchanged fields
}
```

Drop the manual `impl Debug for ProviderConfig` at
`provider_config.rs:169-182`. `SecretString`'s own `Debug` prints
`[REDACTED]`, so a derived `Debug` is now safe. Keep
`#[serde(skip_serializing)]` so a config dump never serializes the key
even though `SecretString` implements `Serialize`.

`crates/llm-proxy-core/src/provider_registry.rs`:
`ProviderAdapterTargetConfig.api_key: SecretString` (`:93`). Drop the
manual `Debug` at `:107-121`; derive it.

`crates/llm-proxy-provider/src/adapter/mod.rs`:
`ProviderAdapterTarget.api_key: SecretString` (`:112`). Drop the manual
`Debug` at `:126-140`; derive it.

### Read sites

Every current `api_key` field read must be updated. The complete
inventory (verified by workspace-wide grep):

**Validation (check non-emptiness / env-var refs):**
- `crates/llm-proxy-core/src/provider_config.rs:811` —
  `find_unresolved_env_var(&provider.api_key)`. Change to
  `find_unresolved_env_var(provider.api_key.expose_secret())`.
- `crates/llm-proxy-core/src/provider_config.rs:817` —
  `provider.api_key.trim().is_empty()`. Change to
  `provider.api_key.expose_secret().trim().is_empty()`. The validation
  error message must not include the key value (it does not today).

**Move/clone between config layers (stays as SecretString clone, no expose):**
- `crates/llm-proxy-core/src/provider_registry.rs:401` —
  `api_key: provider.api_key.clone()` (ProviderConfig ->
  ProviderAdapterTargetConfig). No change needed beyond the field type;
  `SecretString` clone is a refcount bump.
- `crates/llm-proxy-server/src/routes/core_pipeline.rs:422` —
  `api_key: adapter_target_config.api_key` (move into
  ProviderAdapterTarget). No change needed; it's a move of the
  `SecretString`.

**Auth-header construction (expose here):**
- `crates/llm-proxy-provider/src/adapter/mod.rs:490` —
  `api_key: target.api_key.clone()` into `AuthHeaders`. This is where
  `AuthHeaders` is constructed, NOT `transport.rs` as one might expect.
  `AuthHeaders.api_key` (defined at `transport.rs:40`) should ALSO become
  `SecretString` so the secret stays wrapped until the last possible
  moment. Change the field type and the construction to clone the
  `SecretString` directly (no expose here).
- `crates/llm-proxy-provider/src/transport.rs:378,381,384,387,388` —
  `auth.api_key` read in `apply_auth` to build reqwest header values.
  This is the true transport boundary. Change each to
  `auth.api_key.expose_secret()` to get `&str` for the reqwest header
  value. The exposed `&str` must not be logged; it flows only into the
  reqwest header value.

**Discovery requests (expose here — bypasses AuthHeaders):**
- `crates/llm-proxy-provider/src/discovery.rs:182-187` — reads
  `provider.api_key` DIRECTLY for `bearer_auth`/`x-api-key`/
  `x-goog-api-key` headers on discovery requests, bypassing `AuthHeaders`
  entirely. This is a second transport boundary. Change to
  `provider.api_key.expose_secret()`. This site is the most dangerous
  miss if overlooked: it will not compile after the type change, and it
  is NOT covered by the `AuthHeaders` path.

Every other touch is a move or clone of the `SecretString` itself, which
is cheap (it wraps an `Arc`-backed `Zeroizing<String>` in the `alloc`
feature, so clone is a refcount bump).

**Note on `expose_secret()` return type**: `SecretString::expose_secret()`
returns `&Zeroizing<String>`, not `&str`. Deref coercion (`&Zeroizing
<String>` → `&String` → `&str`) makes it work in most contexts (e.g.
`bearer_auth(...)`, `header(...)`, `.trim()`, `.is_empty()`). Do NOT
call `.to_string()` on the exposed value — that would clone the secret
unnecessarily. Use `&**provider.api_key.expose_secret()` if you need an
explicit `&str` in a context where deref coercion doesn't apply.

### AuthHeaders field type

`AuthHeaders.api_key` (`transport.rs:40`) should also become
`SecretString`, not stay `String`. Rationale: `AuthHeaders` is held in
`ProxyRequest` which may be logged or debug-printed; keeping the key as
`SecretString` ensures redaction until `apply_auth` calls
`.expose_secret()` at the reqwest header-set call. The manual Debug
redaction on `AuthHeaders` (if any) can then be replaced with a derive.

### Env interpolation

`env_interpolate.rs` interpolates `${VAR}` in string values. Verified:
`load_provider_config` (`provider_config.rs:1164`) interpolates the raw
TOML string BEFORE `toml::from_str` (`interpolate_env_vars(&raw)` at
`:1211`, `toml::from_str(&interpolated)` at `:1220`). So `${VAR}` is
resolved before serde builds the `SecretString`. No extra work needed;
the existing interpolation hook covers `api_key` because it runs on the
whole raw string. The `load_app_config` path (`:1075`) follows the same
pattern (`:1096` interpolate, `:1109` deserialize).

### Tests

- `ProviderConfig` Debug output contains `[REDACTED]` and never the key.
- `ProviderAdapterTargetConfig` and `ProviderAdapterTarget` Debug redact.
- `AppState` Debug still redacts transitively (existing regression test
  `app_state_debug_does_not_leak_api_key` at `state.rs:290` must stay
  green; if it breaks because the manual impl was removed, fix the test
  to assert the derived redaction).
- A config with `api_key = "${LLM_PROXY_TEST_KEY}"` and that env var set
  loads to a `SecretString` whose `expose_secret()` equals the env value.
- Validation rejects an empty `api_key` without printing it.

### Literal construction sites that must be updated (compile-critical)

Changing `api_key: String` to `api_key: SecretString` breaks every Rust
struct literal that constructs `ProviderConfig` or
`ProviderAdapterTargetConfig` with `api_key: "test-key".to_owned()`.
The complete inventory (verified by workspace grep):

**`ProviderConfig` / `ProviderAdapterTargetConfig` literals (9 sites):**
- `crates/llm-proxy-server/tests/integration.rs:58`
- `crates/llm-proxy-server/tests/chat_completions.rs:149,408,1885,1908`
- `crates/llm-proxy-server/tests/core_pipeline.rs:165,1288,1394,1724`
- `crates/llm-proxy-server/src/state.rs:294,394,437,552,575` (in-crate
  unit tests)

Each must change `api_key: "test-key".to_owned()` to
`api_key: secrecy::SecretString::from("test-key")` (or
`secrecy::SecretString::new("test-key".to_owned())`). Add `secrecy` to
`llm-proxy-server`'s `[dev-dependencies]` if not already a regular dep.

**`ServerConfig` literals that break when `log_format` is added (Step 4):**
- `crates/llm-proxy-server/src/state.rs:227`
- `crates/llm-proxy-server/src/routes/core_pipeline.rs:1387,1554,1622`
- `crates/llm-proxy-server/src/routes/models.rs:263,305`
- `crates/llm-proxy-server/tests/integration.rs:23,83,886`
- `crates/llm-proxy-server/tests/chat_completions.rs:172,365,432,1923`
- `crates/llm-proxy-server/tests/core_pipeline.rs:188,1313,1419,1749`

Each must add `log_format: LogFormat::default()` (or `LogFormat::Plain`).
`#[serde(default)]` helps TOML deserialization but does NOT help Rust
struct literals — every literal must be updated or it won't compile.

**TOML fixture files (OK — no update needed):**
- `crates/llm-proxy-core/src/provider_config.rs:1252,1275,1294,1321,1348`
  — inline TOML strings parsed via `toml::from_str`. `#[serde(default)]`
  on `log_format` and `pricing` handles missing fields in TOML. No
  update needed.
- `config.toml.example` and `providers/*.toml.example` — should be
  updated to document the new fields (see "Files needing documentation
  updates" below).

## Step 1 — Real upstream message IDs (buffer first event)

### Scope

Only `crates/llm-proxy-server/src/routes/core_pipeline.rs` changes. The
protocol encoders and provider adapters already carry the upstream id;
the route just has to use it.

### Change

The current streaming architecture constructs the `ClientStreamEncoder`
in the **handler** (`core_pipeline.rs:675-676`) and moves it into
`StreamContext` (`:687-701`), which is then moved into a spawned task
that runs `build_sse_output_stream`. The cancellation select lives
**inside** that spawned task (`:892-905`).

To buffer the first event while preserving cancellation, the buffering
must happen **inside the spawned task**, not in the handler. The
encoder construction must also move into the task so it can use the
real upstream id. Concretely:

1. **Do not** construct `ClientStreamEncoder` in the handler at
   `:675-676`. Instead, pass the raw `client_protocol`,
   `core.model.requested`, and a reference to `&core` into `StreamContext`
   (add them as fields).
2. Inside the spawned task, before entering the main decode loop:
   a. **Loop** reading bytes from `byte_stream` and feeding them to
      `sse_framer.push_chunk(...)` until `sse_framer` yields at least
      one complete `SseFrame`. This loop handles partial reads
      (`push_chunk` can return an empty `Vec<SseFrame>` when the chunk
      doesn't complete a frame — keep reading). If `byte_stream` returns
      `None` (stream ended) before any frame, fall back to synthetic id
      and proceed with an empty pre-queue.
   b. Call `provider_decoder.decode_frame(&frame)` on the first
      complete frame. This can return an empty `Vec<CoreEvent>` (e.g.
      the frame was a `:keepalive` comment). If empty, keep reading
      frames (loop back to 2a) until a non-empty `Vec<CoreEvent>` is
      produced or the stream ends.
   c. Inspect the first non-empty `Vec<CoreEvent>`:
      - If the first event is `CoreEvent::MessageStart { id: Some(real),
        .. }`, use `real` as `msg_id`.
      - Otherwise (no `MessageStart`, `id: None`, `Error`, `Ping`,
        etc.), fall back to the synthetic id (`chatcmpl-{uuid}` or
        `msg_{uuid}`).
   d. Construct `ClientStreamEncoder::new(client_protocol, msg_id,
      core.model.requested.clone(), &core)` **inside the task**.
   e. **Encode** the buffered `Vec<CoreEvent>` through the newly
      constructed encoder using `encode_core_event(...)`, producing
      `Vec<Event>` (axum SSE `Event`s). These are the pre-queue items.
      **The pre-queue is `Vec<Event>`, not `Vec<CoreEvent>`** — the
      types must match `output_stream` for `.chain()`.
   f. If the buffered events included an `Error`, feed it to the
      `first_byte_tx` probe as today.
3. The main decode loop then proceeds as today, but the output stream
   is `futures::stream::iter(pre_queue).chain(main_loop_stream)` so the
   pre-queue events are emitted first. No events are lost or duplicated.

**Why inside the task, not the handler**: the cancellation select
(`tokio::select!` on `cancel_clone.cancelled()`, `:892-905`) lives inside
the task. If buffering moved to the handler, a client disconnect during
first-frame wait would not be detected until the upstream responded,
defeating the cancellation. By keeping buffering in the task, the
`select!` covers the first-frame wait too.

**What changes in `StreamContext`** (`core_pipeline.rs:796-805`): add
fields `client_protocol: ClientProtocol`, `requested_model: String`,
`core_ref: CoreRequest` (or the specific fields the encoder needs from
`&core`). Remove the `client_encoder` field (now constructed inside the
task). The `first_byte_tx` probe stays.

### Encoder behavior notes

The two protocol encoders handle `MessageStart` id differently:

- **OpenAI** (`client/openai_chat.rs:643`): `CoreEvent::MessageStart { .. }`
  is ignored — the encoder never reads the id from the event. It uses
  only the `id` passed to `new()`, and emits it in every chunk via
  `make_chunk` (`:887,903,917`). So building the encoder with the real
  upstream id is **essential** for OpenAI; the buffered approach is the
  only way to get the real id onto the wire.
- **Anthropic** (`client/anthropic.rs:646-648`): the encoder **overwrites**
  `self.msg_id` from `CoreEvent::MessageStart { id: Some(..) }` when it
  sees the event. So for Anthropic, building with the synthetic id and
  then feeding the buffered `MessageStart` would self-correct anyway.
  The uniform "build with real id" approach works for both and is
  slightly over-specified for Anthropic, but it is harmless and keeps
  the route logic protocol-agnostic.

### Edge cases

- The upstream returns no `MessageStart` at all (some providers open with
  a `Ping` or content). Keep the synthetic id; the pre-queue holds the
  encoded events that did arrive.
- The first frame decodes to an **empty** `Vec<CoreEvent>` (e.g. a
  `:keepalive` comment). Keep reading frames until a non-empty Vec is
  produced or the stream ends. Do not fall back to synthetic on the
  first empty decode — only fall back when the stream ends or an
  `Error` appears.
- The first frame decodes to an `Error` event. Feed it to the
  `first_byte_tx` probe as today; the buffered error event flows
  through the `FirstByteResult::Error` path.
- The upstream closes before any frame. Existing empty-stream handling
  applies; no id is needed.
- The client disconnects during first-frame wait. Because buffering is
  inside the spawned task, the `tokio::select!` on
  `cancel_clone.cancelled()` covers this — the task aborts cleanly.
- Multiple `CoreEvent`s in the first non-empty decode (e.g.
  `MessageStart` + `ContentStart` in one frame). All are encoded into
  the pre-queue; no events are lost.

### Tests

- A mocked upstream whose first event is `MessageStart { id: Some("msg_real_123") }`
  produces client SSE with `id: "msg_real_123"` (Anthropic) or
  `id: "chatcmpl-abc123"` (OpenAI — the upstream id is used **verbatim**,
  not re-prefixed; the synthetic fallback adds `chatcmpl-` but the real
  id already has it).
- A mocked upstream with `MessageStart { id: None }` falls back to the
  synthetic `msg_`/`chatcmpl-` id.
- A mocked upstream that opens with `Ping` then `MessageStart` still
  surfaces the real id (the pre-queue drains correctly).
- A mocked upstream whose first frame is a `:keepalive` comment (empty
  decode) followed by `MessageStart` still surfaces the real id.
- No events are lost or duplicated when the first non-empty frame
  contains multiple events.
- Client disconnect during first-frame wait does not hang the task
  (cancellation fires).

## Step 2 — Real token counting (tiktoken-rs)

### Dependency

Add to `[workspace.dependencies]`:

```toml
tiktoken-rs = "0.6"
```

Add `tiktoken-rs = { workspace = true }` to `llm-proxy-core`. The crate
downloads/loads BPE ranks; confirm the version exposes
`cl100k_base()`, `o200k_base()`, `p50k_base()`, and `r50k_base()`
constructors returning a `Result` (handle load failure by falling back to
the heuristic).

### Design

Introduce a `Tokenizer` trait in a new
`crates/llm-proxy-core/src/token/tiktoken.rs` (sibling to `counter.rs`),
keeping the public `Counter` as the front door:

```rust
/// Token-counting backend.
///
/// `Debug` is required so that `Counter` (which holds `Arc<dyn
/// Tokenizer>`) can derive `Debug`.
pub trait Tokenizer: Send + Sync + std::fmt::Debug {
    /// Count tokens in a single text string.
    fn count_tokens(&self, text: &str) -> usize;
}

/// Heuristic backend (~4 chars/token). Zero-allocation, infallible.
#[derive(Debug, Clone, Default)]
pub struct HeuristicTokenizer;

/// BPE backend using a specific tiktoken encoding.
///
/// `CoreBPE` is `Send + Sync` in tiktoken-rs 0.6 (verified against the
/// crate's impl; if a future version regresses, wrap in `Mutex`). Wrapped
/// in `Arc` so `Counter` is cheap to clone.
#[derive(Debug, Clone)]
pub struct TiktokenTokenizer {
    bpe: Arc<tiktoken_rs::CoreBPE>,
}
```

`Counter` holds a map of model-id-prefix to `Arc<dyn Tokenizer>`, plus
the heuristic fallback:

```rust
/// Token counter with model-aware BPE dispatch.
///
/// No longer a ZST after this change. `AppState` stores it inline; the
/// derived `Clone` on `AppState` handles the `Arc` refcount bump. The
/// old ZST doc comment at `state.rs:64-70` must be updated.
#[derive(Debug, Clone)]
pub struct Counter {
    /// Heuristic fallback for unknown models.
    heuristic: HeuristicTokenizer,
    /// Model-prefix -> BPE tokenizer. Looked up by longest matching
    /// prefix of the model id.
    encodings: HashMap<String, Arc<dyn Tokenizer>>,
}
```

**The `count_messages` signature must change to accept a model id** so
the counter can dispatch to the right encoding. This is the one call-site
break in the plan:

```rust
pub fn count_messages(
    &self,
    model: &str,
    system: &str,
    messages: &[MessageContent],
) -> usize
```

The call site at `token_count.rs:212` (`state.token_counter.count_messages
(&system_text, &messages)`) must become `state.token_counter
.count_messages(&core.model.requested, &system_text, &messages)`. The
`core: CoreRequest` is in scope at that point (`token_count.rs:131`
decodes it), so `core.model.requested` is available.

`Counter::Default` can no longer be derived (HashMap has no Default for
`Arc<dyn Tokenizer>` values — actually `HashMap::default()` is an empty
map, which works). Verify: `HashMap<String, Arc<dyn Tokenizer>>:
Default` is `HashMap::default()` (empty map) — this is fine. So
`#[derive(Default)]` on `Counter` works if `HeuristicTokenizer: Default`
(it does). **But** `Arc<dyn Tokenizer>` does not implement `Default`,
and `#[derive(Default)]` only requires `Default` on fields, not on
trait objects inside a `HashMap` (the `HashMap` itself is `Default`).
So `#[derive(Default)]` on `Counter` is valid. Confirm during
implementation.

The `count_tokens` method also gains a `model` parameter:

```rust
pub fn count_tokens(&self, model: &str, text: &str) -> usize
```

### Model-to-encoding selection

Add a `fn encoding_for_model(model: &str) -> Option<tiktoken_rs::...>` in
`tiktoken.rs` mirroring tiktoken's Python `encoding_for_model` mapping:

- `gpt-4o`, `gpt-4o-*`, `o1-*`, `o3-*` -> `o200k_base`
- `gpt-4`, `gpt-3.5-turbo`, `gpt-4-*`, `gpt-3.5-*` -> `cl100k_base`
- `text-davinci-003`, `text-davinci-002`, `code-*`, `text-*` (legacy) ->
  `p50k_base`
- everything else -> `None` (fall back to heuristic)

For non-OpenAI providers (Anthropic, Gemini, Fireworks, etc.), there is
no public tiktoken encoding. Document this: the proxy counts with the
OpenAI encoding that most closely matches the model family when known,
otherwise the heuristic. Anthropic and Gemini do not publish tokenizers;
provider-reported `Usage` (already captured in `CoreEvent::UsageDelta`)
remains the authoritative count for billing, and the local counter is
only for the `/v1/messages/count_tokens` estimate and pre-flight
guardrails.

### Integration points

- `crates/llm-proxy-server/src/routes/token_count.rs:86`
  (`count_tokens_inner`) — unchanged call to `Counter::count_messages`,
  but the counter now dispatches to BPE. The route's response doc
  (`:23-30`) should be updated: stop calling it a rough approximation for
  known OpenAI models; keep the approximation caveat for unknown models.
  The current exclusion of tools/images (`:135-144`) stays; tiktoken
  counts text only.
- `crates/llm-proxy-server/src/state.rs:70` — `AppState.token_counter`
  stays a `Counter`; construct it once at startup. If BPE load is eager,
  do it in `AppState::new` and propagate a load error to server startup.
  If lazy, the first count for a known model pays a one-time cost;
  document the tradeoff and pick lazy to keep startup fast and avoid
  failing the server when ranks download is unavailable offline.

### Fallback behavior

If an encoding constructor fails at runtime (missing ranks, offline),
log a `warn!` once and use the heuristic for that encoding. The proxy
must never fail a request because the tokenizer could not load; the
heuristic is always available.

### tiktoken-rs specifics (resolved)

**Rank loading**: `tiktoken-rs` 0.6 bundles BPE ranks at **compile time**
via `include_bytes!` for the standard encodings (`cl100k_base`,
`o200k_base`, `p50k_base`, `r50k_base`). No network fetch at runtime.
This means the binary grows by ~2-4 MB per encoding (the rank files are
gzip-compressed and decompressed on first use). Offline operation is
fully supported. Verify on first `cargo build` that no network fetch
occurs; if a future `tiktoken-rs` version changes this, the plan's
lazy-load + `warn!` fallback covers it.

**Send + Sync**: `tiktoken_rs::CoreBPE` is `Send + Sync` (its fields are
`HashMap<Vec<u8>, (Vec<u8>, usize)>` and `HashMap<(Vec<u8>, Vec<u8>),
usize>` — both `Send + Sync`). So `Arc<CoreBPE>` is directly shareable
across tasks with no `Mutex`. If a future version regresses, wrap in
`Arc<Mutex<CoreBPE>>` and document the lock contention (serializes all
token counts across requests, which is acceptable for a count-only
operation that is not on the inference hot path).

**Load timing**: Lazy on first `count_tokens` call for a known model
prefix. The first request for that model pays a one-time decompression
cost (~1-5 ms). Subsequent requests use the cached `Arc<CoreBPE>`. This
keeps server startup fast and avoids failing the server when a rank
file is corrupt (the failure is per-encoding, not server-wide).

### Tests

- Known OpenAI model ids select the expected encoding.
- Unknown model ids fall back to heuristic.
- A known model's BPE count for a fixture string matches a recorded
  tiktoken count (use a stable fixture, e.g. "hello world" -> known
  token count for `cl100k_base`).
- `count_messages` with a known model is within a small tolerance of the
  provider-reported `Usage.input_tokens` on a captured fixture (golden
  test).
- Heuristic path is unchanged (existing `counter.rs` tests stay green).
- BPE load failure falls back to heuristic without panicking.

## Step 3 — Cost/pricing config and computation

### Types

Add `ModelId` and `ModelPricing` to
`crates/llm-proxy-core/src/provider_config.rs`:

```rust
/// Provider-local model identifier used as a pricing key.
///
/// Transparent newtype over `String` so it serializes as a plain string
/// in TOML/JSON. Intentionally minimal: existing `String` model fields
/// are not migrated to this type in this plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(
    /// The model id string.
    pub String,
);

impl ModelId {
    pub fn new(id: impl Into<String>) -> Self { Self(id.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
}

/// Per-token price for one model, in USD.
///
/// All fields are USD per token as `rust_decimal::Decimal` to avoid
/// floating-point money. A price of `0` means "unmetered".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPricing {
    /// USD per input token.
    pub input: Decimal,
    /// USD per output token.
    pub output: Decimal,
    /// USD per cache-creation input token.
    #[serde(default)]
    pub cache_creation: Decimal,
    /// USD per cache-read input token.
    #[serde(default)]
    pub cache_read: Decimal,
    /// USD per reasoning token (OpenAI/Responses-style).
    #[serde(default)]
    pub reasoning: Decimal,
}
```

Add the `pricing` field to `ProviderConfig`:

```rust
pub struct ProviderConfig {
    // ...existing fields
    /// Per-model pricing, keyed by upstream model id. Used for cost
    /// estimation; not enforced for routing. Aliases are resolved before
    /// lookup, so the key is the upstream model id.
    #[serde(default)]
    pub pricing: HashMap<ModelId, ModelPricing>,
}
```

### Dependency

Add to `[workspace.dependencies]`:

```toml
rust_decimal = "1"
```

Add `rust_decimal = { workspace = true }` to `llm-proxy-core` (config)
and `llm-proxy-protocol` (cost on `Usage`). `rust_decimal` is pure-Rust,
no float.

### TOML shape

```toml
[provider.pricing."accounts/fireworks/models/deepseek-v3p1"]
input = "0.0000014"   # USD per token
output = "0.0000028"

[provider.pricing."gpt-4o"]
input = "0.0000025"
output = "0.000010"
cache_creation = "0.000003"
cache_read = "0.00000125"
reasoning = "0.000010"
```

`Decimal` serializes as a string to preserve precision. **Critical
usability note**: if an operator writes `input = 0.0000014` (unquoted),
TOML parses it as `f64` (losing precision), and `rust_decimal::Decimal`'s
`Deserialize` impl **rejects** floats — deserialization fails with a
confusing error. Operators must **always quote** decimal values:
`input = "0.0000014"`. Document this prominently in `config.toml.example`,
`defaults.rs`, and the provider examples. Consider adding a custom
deserializer that accepts both string and float (coercing via
`Decimal::try_from(f64)`) as a follow-up; for this plan, the "always
quote" documentation is sufficient.

### Cost computation

Add to `crates/llm-proxy-protocol/src/core.rs` next to `Usage`:

```rust
/// Computed cost for one request, in USD.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cost {
    /// Input cost.
    pub input: Decimal,
    /// Output cost.
    pub output: Decimal,
    /// Cache-creation cost.
    pub cache_creation: Decimal,
    /// Cache-read cost.
    pub cache_read: Decimal,
    /// Reasoning cost.
    pub reasoning: Decimal,
    /// Total = input + output + cache_creation + cache_read + reasoning.
    pub total: Decimal,
}

impl Cost {
    pub fn from_usage(usage: &Usage, pricing: &ModelPricing) -> Self { ... }
}
```

`Cost::from_usage` multiplies each `Usage` field by the matching
`ModelPricing` field using `Decimal` arithmetic and sums to `total`.
`Usage` itself is unchanged (it stays the provider-reported token counts);
`Cost` is a derived value computed at the pipeline boundary where
`Usage` is final. The full body:

```rust
impl Cost {
    pub fn from_usage(usage: &Usage, pricing: &ModelPricing) -> Self {
        let input = Decimal::from(usage.input_tokens) * pricing.input;
        let output = Decimal::from(usage.output_tokens) * pricing.output;
        let cache_creation = Decimal::from(
            usage.cache_creation_input_tokens.unwrap_or(0),
        ) * pricing.cache_creation;
        let cache_read = Decimal::from(
            usage.cache_read_input_tokens.unwrap_or(0),
        ) * pricing.cache_read;
        let reasoning = Decimal::from(
            usage.reasoning_tokens.unwrap_or(0),
        ) * pricing.reasoning;
        let total = input + output + cache_creation + cache_read + reasoning;
        Self { input, output, cache_creation, cache_read, reasoning, total }
    }
}
```

`Option<i32>` fields use `unwrap_or(0)` — if the provider didn't report
cache/reasoning tokens, they contribute zero cost. Negative `Usage`
fields (theoretical with cache-adjustment deltas) produce negative
costs; this is intentional (refund/adjustment semantics), not clamped.

### Integration

- `crates/llm-proxy-core/src/provider_registry.rs` — expose a
  `pricing_for(&self, provider: &str, upstream_model: &str) ->
  Option<&ModelPricing>` accessor on `ProviderRegistry` so the pipeline
  can look up pricing after alias resolution (the upstream model id is
  the key, per locked decision 8). **Borrow lifetime note**: the
  returned `&ModelPricing` borrows from `Arc<ProviderRegistry>` in
  `AppState`. If the pipeline holds it across `.await` points while
  reborrowing `AppState`, borrowck can conflict. The safest pattern is
  to `cloned()` the `ModelPricing` (cheap: 5 `Decimal`s = 80 bytes)
  immediately after lookup, before any `.await`, and pass the owned
  `ModelPricing` into the stream/task.
- `crates/llm-proxy-server/src/routes/core_pipeline.rs` — in both
  `handle_core_once` (`:452`) and `handle_core_stream` (`:596`), once the
  final `Usage` is known, compute `Cost::from_usage` if pricing exists,
  attach to the response/event, and feed to the event log (Step 4). If
  no pricing is configured for the model, `Cost` is `None` / zero and the
  response is unchanged.
  - **Non-stream path**: `Usage` comes from `CoreResponse.usage` at the
    success tail of `handle_core_once`.
  - **Stream path**: `CoreEvent::UsageDelta` (`core.rs:954`) can be
    emitted multiple times; the Anthropic client encoder buffers the
    latest (`client/anthropic.rs:599-602`). Cost must be computed at the
    `MessageStop` boundary (`core.rs:959`) — after the last `UsageDelta`
    has been seen — not inline at each `UsageDelta`. Wire the cost
    computation into the `MessageStop` tail of
    `build_sse_output_stream`, using the latest buffered `Usage`.
    **StreamContext additions** (`core_pipeline.rs:796-805`): add
    `pending_usage: Option<Usage>` (updated on every `UsageDelta`),
    `pricing: Option<ModelPricing>` (cloned from `pricing_for` before
    the stream starts), `event_bus: Arc<dyn EventBus>`, `request_id:
    String`, `provider_name: String`, `upstream_message_id:
    Option<String>`, `start: Instant` (for `latency_ms`). The decode
    loop (`:969-979`) must match on `CoreEvent::UsageDelta` to update
    `pending_usage` and on `CoreEvent::MessageStop` to compute
    `Cost::from_usage(pending_usage, pricing)`, emit `ResponseCompleted`
    via `event_bus.emit(&event)`, and then proceed with the normal
    `MessageStop` encoding. The encoder's internal `pending_usage`
    (Anthropic) is private and not accessible to the route — the route
    needs its own accumulator.
- `CoreResponse` (`core.rs:747`) gains an optional `cost: Option<Cost>`
  field, serialized as `cost` in the normalized core response. The client
  encoders (`client/anthropic.rs`, `client/openai_chat.rs`) are not
  required to surface it on the wire (OpenAI/Anthropic don't have a cost
  field); it is available to the event log and the future API crate.
  Keep `#[serde(skip_serializing_if = "Option::is_none")]` so existing
  wire shapes are byte-identical when no pricing is configured.
  - **Doc the field**: `/// Computed cost, if pricing was configured.
    None when no pricing is configured for the model.` — the protocol
    crate has `#![deny(missing_docs)]` (`lib.rs:7`), so the field MUST
    have a `///` or the build fails.
  - **Update `CoreResponse`'s manual `Debug`** (`core.rs:769-781`): add
    `.field("cost", &self.cost)` so cost is visible in debug output.
    Without this, cost is silently invisible in debug (not a compile
    error, but a debugging hazard for a billing-relevant field).
  - **Read ordering in `handle_core_once`** (`core_pipeline.rs:501-527`):
    `core_resp` is **moved** into `encode_response` at `:516`/`:522`.
    The `Cost` and `Usage` for the event log must be read **before** that
    move. Compute `let cost = Cost::from_usage(&core_resp.usage, ...)`
    at `:507` (before encode), then set `core_resp.cost = Some(cost
    .clone())` before the move, and pass `cost` to the event log after
    the move. The `Cost` clone is cheap (5 `Decimal`s = 5 × 16 bytes).

### Validation

`validate_provider_config` (`provider_config.rs:789`) gains:

- Every `ModelPricing` field is non-negative.
- `ModelId` keys are non-empty.
- A pricing key colliding with a `model_aliases` value (upstream id) is
  allowed; colliding with an alias *source* is a warning, not an error
  (the source is a client-facing name, not a pricing key).

### Tests

- `ModelPricing` parses from TOML with string decimals.
- `Cost::from_usage` matches a hand-computed fixture.
- Negative price rejected by validation.
- Empty `ModelId` rejected.
- Provider with no `pricing` block produces `Cost = None` and the wire
  response is byte-identical to today.
- Alias resolution: a request for alias `kimi` resolving to
  `accounts/fireworks/models/kimi-k2.6` looks up pricing under the
  upstream id, not the alias.

## Step 4 — Structured request/response event log

### Two sinks, staged

**Phase A — JSON tracing layer (ops logs).** Add the `json` feature to
`tracing-subscriber` and gate the layer on a new `ServerConfig.log_format`
field (`"plain"` default, `"json"` opt-in). This gives structured
JSON logs for ops without any new domain types.

**Phase B — `EventBus` trait + event types (persistence sink).** Define
the trait and event structs in `crates/llm-proxy-storage` so the eventual
SQLite backend can implement it. Emit events at the pipeline boundaries.
In this plan the only `EventBus` implementation is an in-memory
`RecordingBus` for tests and a `NoopBus` default; the real persisted
backend lands in the storage phase.

### Phase A: JSON tracing layer

`Cargo.toml` workspace deps — extend the `tracing-subscriber` features:

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
```

`crates/llm-proxy-core/src/provider_config.rs` — add to `ServerConfig`:

```rust
/// Log output format: "plain" (default) or "json".
#[serde(default)]
pub log_format: LogFormat,
```

with

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Plain,
    Json,
}

impl LogFormat {
    /// Read `RUST_LOG_FORMAT` env var. Defaults to `Plain`.
    pub fn from_env() -> Self {
        match std::env::var("RUST_LOG_FORMAT")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "json" => Self::Json,
            _ => Self::Plain,
        }
    }
}
```

`apps/llm-proxy/src/state.rs:129` (`init_tracing`) — accept a `LogFormat`
and select the layer. The function currently takes no args; change it to
`init_tracing(log_format: LogFormat)`. Note the existing doc
(`state.rs:125-128`) says the subscriber must init before config is
parsed, so `log_format` cannot come from the TOML at init time. Resolve
this by reading `RUST_LOG_FORMAT` from the env at init (consistent with
`RUST_LOG`), and let the TOML `log_format` field be a documented default
that the operator sets and the env var overrides. `cmd_serve` calls
`init_tracing(LogFormat::from_env())` before loading config.

```rust
pub fn init_tracing(log_format: LogFormat) {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::builder().parse_lossy("info"));
    let registry = tracing_subscriber::registry().with(filter);
    match log_format {
        LogFormat::Plain => registry.with(fmt::layer()).init(),
        LogFormat::Json  => registry.with(fmt::layer().json()).init(),
    }
}
```

### Phase B: EventBus trait and event types

`crates/llm-proxy-storage/src/lib.rs` — replace the stub with the event
surface. Keep `placeholder` if anything still references it, or remove it
if nothing does (grep first).

```rust
//! Structured request/response event log surface.
//!
//! The persistence backend (SQLite, etc.) lands in a later phase. This
//! crate defines the event types and the EventBus trait that backends
//! implement, plus a NoopBus default and a RecordingBus for tests.

use std::sync::Arc;

use llm_proxy_protocol::core::{Cost, ModelRef, StopReason, Usage};
use rust_decimal::Decimal;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// One request/response lifecycle event, ready to persist or emit.
///
/// Internally tagged via `kind` for JSON/TOML consumers. Every field on
/// every variant has a `///` doc comment because `missing_docs = "warn"`
/// (workspace) is promoted to deny by the plan's `cargo clippy -- -D
/// warnings` gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyEvent {
    /// A request was received and routed.
    RequestReceived(RequestReceived),
    /// A response completed successfully.
    ResponseCompleted(ResponseCompleted),
    /// A response failed before or during upstream dispatch.
    ResponseFailed(ResponseFailed),
}

/// Event payload for a received request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestReceived {
    /// Proxy-generated unique request identifier.
    pub request_id: String,
    /// When the request was received (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Provider name from the URL path.
    pub provider: String,
    /// Route kind (`"chat_completions"` or `"messages"`).
    pub route_kind: String,
    /// Client protocol (`"openai_chat"` or `"anthropic"`).
    pub client_protocol: String,
    /// Requested and upstream model.
    pub model: ModelRef,
    /// Whether the request requested streaming.
    pub streaming: bool,
    /// SHA-256 of the raw client request body for dedup/correlation
    /// (not the body itself, which may contain secrets).
    pub body_hash: String,
}

/// Event payload for a completed response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseCompleted {
    /// Proxy-generated unique request identifier.
    pub request_id: String,
    /// When the response completed (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Provider name.
    pub provider: String,
    /// Upstream message id, if available from `MessageStart` or
    /// `CoreResponse.id`.
    pub upstream_message_id: Option<String>,
    /// Requested and upstream model.
    pub model: ModelRef,
    /// Token usage reported by the provider.
    pub usage: Usage,
    /// Computed cost, if pricing was configured for the model.
    pub cost: Option<Cost>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// Request latency in milliseconds.
    pub latency_ms: u64,
}

/// Event payload for a failed response.
///
/// `model` and `provider` are `Option` because early failures (unknown
/// provider, JSON parse error, rate limit) can occur before the model
/// or provider is confirmed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFailed {
    /// Proxy-generated unique request identifier.
    pub request_id: String,
    /// When the failure occurred (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Provider name, if known at failure time.
    pub provider: Option<String>,
    /// Requested and upstream model, if decoded before failure.
    pub model: Option<ModelRef>,
    /// Error kind string from the `RouteError` variant name.
    pub error_kind: String,
    /// Sanitized error message; never includes api keys or raw upstream
    /// bodies.
    pub message: String,
    /// HTTP status code returned to the client.
    pub http_status: u16,
    /// Request latency in milliseconds.
    pub latency_ms: u64,
}

/// In-process event sink. Implementations: `NoopBus`, `RecordingBus`
/// (tests), and the future SQLite backend.
///
/// `Debug` is a super-trait so that `Arc<dyn EventBus>` can be formatted
/// in `AppState`'s manual `Debug` impl (`state.rs:94-108`). Without it,
/// the manual `Debug` cannot compile.
pub trait EventBus: Send + Sync + std::fmt::Debug {
    /// Emit one event. Must not block the calling task; backends do IO
    /// on a dedicated writer task.
    fn emit(&self, event: &ProxyEvent);
}

/// Default no-op sink.
#[derive(Debug, Clone, Default)]
pub struct NoopBus;
impl EventBus for NoopBus {
    fn emit(&self, _event: &ProxyEvent) {}
}

/// Test-only sink that records events for assertions. Available behind
/// the `test-utils` cargo feature (NOT `#[cfg(test)]`, which is stripped
/// when the crate is compiled as a dependency — `llm-proxy-server` tests
/// need to import it).
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Default)]
pub struct RecordingBus {
    /// Recorded events, guarded by a mutex for interior mutability.
    pub events: std::sync::Mutex<Vec<ProxyEvent>>,
}
```

### Wiring

`crates/llm-proxy-server/src/state.rs` — add `event_bus: Arc<dyn EventBus>`
to `AppState`, defaulting to `Arc::new(NoopBus)`. `cmd_serve` constructs
the real bus (still `NoopBus` in this plan; the persisted bus lands later).

**AppState Debug + constructor updates (compile-critical):**

- `AppState` has a manual `impl Debug` (`state.rs:94-108`) that calls
  `.field(name, &self.field)` for every field. Because `EventBus` now
  requires `Debug` (see trait definition above), `Arc<dyn EventBus>`
  formats correctly. Add `.field("event_bus", &self.event_bus)` to the
  manual `Debug` impl.
- The regression test `app_state_debug_covers_all_fields` (`state.rs:335`)
  iterates a hardcoded field-name list — **must add `"event_bus"`** or it
  fails.
- `AppState::new` (`state.rs:114`) and `AppState::new_with_catalog_dir`
  (`state.rs:133-157`) both construct `AppState` as a struct literal.
  **Both must initialize `event_bus: Arc::new(NoopBus)`** or the literal
  won't compile. `AppState::new` delegates to `new_with_catalog_dir`, so
  only the latter needs the field added; but verify the delegation chain.
- `llm-proxy-server/Cargo.toml` must add `llm-proxy-storage` as a regular
  dependency (for `NoopBus`/`EventBus`) and as a dev-dependency with
  `features = ["test-utils"]` (for `RecordingBus` in integration tests).

`crates/llm-proxy-server/src/routes/core_pipeline.rs` — emit:

- `RequestReceived` at the end of `prepare_request` (`:224`), after the
  request id and provider are known. `body_hash` is computed over the
  **raw client `body: &[u8]`** passed to `prepare_request` (the raw body
  is available there — it has NOT yet been decoded into `CoreRequest`;
  decoding happens after `prepare_request`, e.g. `chat.rs:81-90`). Hash
  with `sha2::Sha256` (already a dep of `llm-proxy-server`). Never
  include the body itself in the event.
- `ResponseCompleted` at the success tail of `handle_core_once` and at
  the `MessageStop` tail of `build_sse_output_stream`. Includes `usage`,
  `cost` (from Step 3), `upstream_message_id` (from Step 1),
  `stop_reason`, `latency_ms`.
- `ResponseFailed` at every `RouteError` return in the pipeline, with
  `error_kind` from the `RouteError` variant and `http_status` from
  `map_upstream_status` / the route error's status mapping.

The emit calls must never block the hot path. `EventBus::emit` takes
`&ProxyEvent` (not by value) so `NoopBus` does not force the caller to
allocate. Callers construct `ProxyEvent` on the stack and pass `&event`;
if the bus is `NoopBus`, the only cost is the stack construction (which
the compiler can elide if the event is unused after `emit`). For the
future SQLite backend, the `emit` impl clones the event into a channel
and returns immediately; the writer task does the IO.

### Dependencies

- `sha2` is already a direct dep of `llm-proxy-server`
  (`crates/llm-proxy-server/Cargo.toml:31`, version `0.10`, in
  `Cargo.lock` as `0.10.9`). No workspace-deps change needed for sha2;
  it is already available for `body_hash`.
- `time` is in workspace deps (`Cargo.toml:39`) but with features
  `["formatting", "parsing"]` only — **no `serde` feature**. The
  `OffsetDateTime` fields in `ProxyEvent` etc. use
  `#[derive(Serialize, Deserialize)]`, which requires `time`'s `serde`
  feature. Furthermore, `time`'s default `serde` impl serializes
  `OffsetDateTime` as an **opaque struct** (unix seconds + nanos +
  offset), not RFC3339 — which is useless for a queryable event log.
  **Must change** `Cargo.toml:39` to:
  `time = { version = "=0.3.44", features = ["formatting", "parsing", "serde", "serde-well-known"] }`.
  The `serde-well-known` feature enables `time::serde::rfc3339`, used via
  `#[serde(with = "time::serde::rfc3339")]` on every `timestamp` field
  (as shown in the event type definitions above). Then add
  `time = { workspace = true }` to `llm-proxy-storage`.
- `llm-proxy-storage` currently has **zero dependencies** (only
  `[package]` + `[lints]`). It now depends on `llm-proxy-protocol` (for
  `Usage`, `Cost`, `ModelRef`, `StopReason`), `rust_decimal`, `time`,
  `secrecy`, and `serde`. No cycle is created: `llm-proxy-protocol` does
  not depend on `llm-proxy-storage`.
- **`test-utils` feature**: add `[features] test-utils = []` to
  `llm-proxy-storage/Cargo.toml`. `RecordingBus` is gated as
  `#[cfg(any(test, feature = "test-utils"))]`. Then
  `crates/llm-proxy-server/Cargo.toml` adds
  `llm-proxy-storage = { workspace = true, features = ["test-utils"] }`
  under `[dev-dependencies]` so integration tests can use `RecordingBus`.
  Regular `[dependencies]` in `llm-proxy-server` uses
  `llm-proxy-storage = { workspace = true }` (no `test-utils`).
- `llm-proxy-protocol/src/lib.rs` has **no `pub use` re-exports** — the
  types `Cost`, `Usage`, `ModelRef`, `StopReason` live at
  `llm_proxy_protocol::core::*`, not the crate root. The storage crate
  must import them as `use llm_proxy_protocol::core::{...}` (as shown in
  the code above), OR re-exports must be added to
  `llm-proxy-protocol/src/lib.rs`. The code above uses the `core::` path
  to avoid modifying the protocol crate's export surface.

### Tests

- `LogFormat::Json` init produces JSON-formatted log lines (capture via
  `tracing_subscriber`'s test layer or a buffer writer).
- `RecordingBus` captures a `RequestReceived` + `ResponseCompleted`
  sequence for a successful non-stream request.
- `RecordingBus` captures a `ResponseFailed` with the right `http_status`
  for an unknown-provider 404.
- Stream request emits `ResponseCompleted` with the real
  `upstream_message_id` from Step 1.
- `body_hash` is stable for identical bodies and differs for different
  bodies.
- Event JSON never contains `api_key` or `SecretString` inner values
  (source-guard test: grep event serialization for known key strings).
- `NoopBus` does not allocate on emit (or at least does not block).

## Module Ownership

`llm-proxy-core`:

- `SecretString` on `ProviderConfig` and `ProviderAdapterTargetConfig`.
- `ModelId`, `ModelPricing`, `pricing` map on `ProviderConfig`.
- `LogFormat` on `ServerConfig` + `LogFormat::from_env()`.
- `Tokenizer` trait (`Send + Sync + Debug`), `TiktokenTokenizer`,
  `HeuristicTokenizer`, model-to-encoding map in `token/`.
- `Counter` signature change: `count_messages` / `count_tokens` gain
  `model: &str` param. No longer a ZST.
- Pricing validation in `validate_provider_config`.
- `pricing_for` accessor on `ProviderRegistry`.
- No HTTP, no event emission.

`llm-proxy-protocol`:

- `Cost` type and `Cost::from_usage` next to `Usage`.
- Optional `cost: Option<Cost>` on `CoreResponse`.
- No tokenizer, no secrets, no event bus.

`llm-proxy-provider`:

- `SecretString` on `ProviderAdapterTarget` and `AuthHeaders`.
- `expose_secret()` at two transport boundaries: `transport.rs`
  (`apply_auth`, lines `378-388`) and `discovery.rs` (`:182-187`).
  Discovery bypasses `AuthHeaders` and reads `provider.api_key` directly.
- No tokenizer, no pricing, no event bus.

`llm-proxy-storage`:

- `ProxyEvent`, `RequestReceived`, `ResponseCompleted`, `ResponseFailed`
  (all fields documented for `missing_docs`).
- `EventBus` trait (`Send + Sync + Debug`), `NoopBus`, `RecordingBus`
  (behind `test-utils` feature).
- `test-utils` cargo feature.
- No persistence backend yet (this plan).

`llm-proxy-server`:

- Buffered-first-event upstream id in `core_pipeline.rs` (inside spawned
  task, not handler).
- `StreamContext` additions: `pending_usage`, `pricing`, `event_bus`,
  `request_id`, `provider_name`, `upstream_message_id`, `start`.
- Tokenizer integration in `token_count.rs` (via `Counter` with model
  param).
- Cost computation at pipeline boundaries (non-stream: before
  `encode_response` move; stream: at `MessageStop` in decode loop).
- `EventBus` in `AppState` (with `Debug` update + constructor update);
  emit calls at pipeline boundaries.
- `body_hash` computation with `sha2` (already a dep).
- `llm-proxy-storage` as regular dep + dev-dep with `test-utils`.

`apps/llm-proxy`:

- `init_tracing(LogFormat)` with JSON layer.
- `RUST_LOG_FORMAT` env override.
- Construct `EventBus` in `cmd_serve` (still `NoopBus` here).

## Skill Mapping

### From `axum-web-framework`

| Section | Use here |
|---|---|
| State management, `Arc<dyn Trait>` in `AppState` | `Arc<dyn EventBus>` in `AppState`; cheap clone across handlers |
| Middleware ordering via `ServiceBuilder` | Unchanged; the `TraceLayer` at `routes/mod.rs:86` continues to wrap the pipeline. JSON tracing layer is a subscriber concern, not a tower layer. |
| Custom extractors / `FromRequestParts` | Not needed; event emission happens inside handlers, not via extractors. |
| Error handling with `IntoResponse` | `ResponseFailed` events emitted at the same `RouteError` sites that already render errors; no new error type. |
| Testing with `tower::ServiceExt::oneshot` | Event-log tests use `oneshot` against `build_router` with a `RecordingBus` in state. |

### From `rust-best-practices`

| Chapter | Application |
|---|---|
| Ch. 1 Borrowing & Ownership | `expose_secret()` returns `&str`; borrow it into the header value, do not clone the secret. `Arc<dyn EventBus>` is cheap to clone. `&ModelPricing` returned from `pricing_for` avoids cloning the price table. |
| Ch. 2 Linting | `cargo clippy --all-targets --all-features --locked -- -D warnings` after every step. Watch `redundant_clone` around `SecretString` clones (they are refcount bumps, not free, but necessary). |
| Ch. 3 Performance | Lazy-load BPE encodings to keep startup fast; cache the loaded `CoreBPE` in an `Arc` so the first count pays the cost and subsequent counts are cheap. Avoid `collect` in the buffered-first-event path. |
| Ch. 4 Error handling | `thiserror` for `Tokenizer`/encoding load errors; fall back to heuristic, never panic. `EventBus::emit` is infallible (`&self`, no `Result`) so a backend failure cannot crash the request path. |
| Ch. 5 Testing | One assertion per test for the event-log sequence. Golden test for tiktoken counts against a recorded fixture. |
| Ch. 6 Generics & Dispatch | `dyn Tokenizer` is acceptable here (counting is not hot enough to matter vs. the BPE work itself). `Arc<dyn EventBus>` is the standard shape for a swappable sink. |
| Ch. 7 Type State | Not needed. `SecretString` already encodes the "secret vs exposed" distinction at the type level via `expose_secret()`. |
| Ch. 8 Docs | `///` on `ModelId`, `ModelPricing`, `Cost`, `ProxyEvent`, `EventBus` — and on **every public field** of each. Workspace `missing_docs = "warn"` (`Cargo.toml:65`), but `llm-proxy-protocol/src/lib.rs:7` overrides to `#![deny(missing_docs)]`, so any undocumented public item in the protocol crate (e.g. `Cost`, `Cost`'s fields) **fails the build immediately**. The storage crate inherits the workspace "warn"; promote to deny once it has real items. Under `cargo clippy -- -D warnings` (the plan's gate), "warn" becomes a hard fail everywhere, so every public field needs a `///` — including `ModelId(pub String)`'s field. |
| Ch. 9 Send/Sync | `EventBus: Send + Sync + Debug` so it can live in `Arc` inside `AppState` and format in the manual `Debug` impl. `TiktokenTokenizer` wraps `Arc<CoreBPE>` (verified `Send + Sync` in tiktoken-rs 0.6). `Tokenizer: Send + Sync + Debug` so `Counter` can derive `Debug`. |

## Test Plan

### Secrets

- `ProviderConfig` / `ProviderAdapterTargetConfig` / `ProviderAdapterTarget`
  / `AuthHeaders` Debug output contains `[REDACTED]`, never the key value.
- `AppState` Debug regression test (`state.rs:290`) stays green.
- Env-interpolated key loads into `SecretString` correctly.
- Validation rejects empty key without printing it.
- Discovery request auth headers (`discovery.rs:182-187`) use
  `expose_secret()` and the key never appears in discovery logs or Debug.
- Transport request auth headers (`transport.rs:378-388`) use
  `expose_secret()` and the key never appears in transport logs or Debug.
- Source-guard: grep all event/log serialization for a known test key
  string; expect zero matches.

### Upstream message IDs

- Mocked `MessageStart { id: Some("msg_real_123") }` surfaces as the
  client SSE id (Anthropic: `msg_real_123`; OpenAI: the upstream id
  verbatim, e.g. `chatcmpl-abc123` — NOT re-prefixed).
- `id: None` falls back to synthetic.
- First-frame `Ping` then `MessageStart` still surfaces the real id.
- First-frame `:keepalive` (empty decode) then `MessageStart` still
  surfaces the real id (loop continues past empty decodes).
- Multi-event first frame: no events lost or duplicated.
- Client disconnect during first-frame wait: task aborts cleanly
  (cancellation select covers the buffering loop).

### Token counting

- Known OpenAI model ids select the expected encoding.
- Unknown ids fall back to heuristic.
- `count_messages` with model param: a known model's BPE count for a
  fixture string matches a recorded tiktoken count (use a stable fixture,
  e.g. "hello world" -> known token count for `cl100k_base`).
- `count_messages` with model param: within a small tolerance of the
  provider-reported `Usage.input_tokens` on a captured fixture (golden
  test).
- Heuristic path is unchanged (existing `counter.rs` tests stay green
  after updating call sites to pass a model param — use an unknown model
  id to hit the heuristic path).
- BPE load failure falls back to heuristic without panicking.
- `Counter` is no longer a ZST but is `Clone` (Arc refcount bump) and
  `Debug` (trait requires Debug).

### Pricing

- `ModelPricing` parses from TOML with quoted string decimals.
- `ModelPricing` with unquoted float TOML value fails with a clear error
  (document the "always quote" rule).
- `Cost::from_usage` matches hand-computed fixture (including
  `Option<i32>` fields that are `None` → 0 cost).
- Negative price rejected by validation; empty `ModelId` rejected.
- No `pricing` block -> `Cost = None` -> wire response byte-identical.
- Alias resolution: pricing looked up under upstream id, not alias.
- `pricing_for` returns `Option<&ModelPricing>`; caller clones before
  crossing `.await` (borrow lifetime test).
- `CoreResponse` manual `Debug` includes the `cost` field.

### Event log

- `LogFormat::Json` produces JSON log lines.
- `LogFormat::from_env()` reads `RUST_LOG_FORMAT` correctly (case-
  insensitive, defaults to `Plain`).
- `RecordingBus` captures `RequestReceived` + `ResponseCompleted` for a
  successful non-stream request.
- `RecordingBus` captures `ResponseFailed` with correct `http_status`
  for a 404 unknown-provider (model and provider are `None` in the
  event).
- Stream request emits `ResponseCompleted` with the real
  `upstream_message_id` from Step 1.
- `body_hash` is SHA-256 of the raw client body (stable for identical
  bodies, differs for different bodies).
- Event JSON timestamps are RFC 3339 format (not opaque structs).
- Event JSON never contains `api_key` or `SecretString` inner values
  (source-guard test: grep event serialization for known key strings).
- `NoopBus` does not force allocation on emit (`emit(&self, &ProxyEvent)`
  — NoopBus drops the reference without cloning).
- `RecordingBus` is importable from `llm-proxy-server` integration tests
  (via `test-utils` feature).

## Verification Commands

Run after every step, and once at the end:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Note: `--all-features` is intentionally omitted. The workspace currently
defines no cargo features. If the `test-utils` feature is added to
`llm-proxy-storage` (for `RecordingBus`), `--all-features` would pull
test-only code into release builds, which is undesirable.

End-of-plan source-guard scans:

```sh
rg "api_key:\s*String" crates apps       # expect zero matches (all SecretString)
rg "expose_secret" crates apps           # expect matches in transport.rs, discovery.rs, validation
rg "Debug for Provider(Config|AdapterTarget"  # expect zero manual impls (derived now)
rg "pricing|ModelPricing|Cost" crates    # expect matches in core/protocol/server
rg "EventBus|ProxyEvent" crates          # expect matches in storage/server
```

## Files Needing Documentation Updates

These files must be updated to document the new config fields. They are
not compile-critical (serde defaults handle missing fields in TOML), but
operators will not know the fields exist without them:

- `config.toml.example` — add `# log_format = "plain"` comment showing
  the option and the `"json"` alternative.
- `apps/llm-proxy/src/defaults.rs:16-25` (`DEFAULT_CONFIG_TOML`) — add
  a `log_format` line or comment so `llm-proxy init` generates configs
  that document the option.
- `providers/opencode-go.toml.example` — add a commented
  `[provider.pricing]` example block.
- `providers/opencode-zen.toml.example` — add a commented
  `[provider.pricing]` example block.
- `apps/llm-proxy/src/commands/validate.rs` — optionally extend the
  route table printout to show pricing entries per provider.

## Deferred Features

- Persisting `ProxyEvent` to SQLite or any backend (storage phase).
- HTTP handlers in `llm-proxy-api` exposing usage/cost/routes (API phase).
- Native upstream token-count endpoints (per-provider).
- Per-client cost quotas, spend limits, budget alerts.
- Hot-reload of pricing or API keys.
- Retrofitting all `String` model fields to `ModelId`.
- Distributed tracing via `tracing-opentelemetry` / OTLP.
- Per-key rate limiting via `governor` (separate from the existing
  per-IP limiter).
- Constant-time API-key comparison via `subtle` (relevant once the proxy
  issues its own inbound keys; the current per-IP limiter is unaffected).

## Open Questions

1. **BPE load timing.** Resolved: lazy on first use, with a one-time
   `warn!` on load failure. `tiktoken-rs` 0.6 bundles ranks at compile
   time via `include_bytes!` — no network fetch, offline-safe. Binary
   grows ~2-4 MB per encoding. See "tiktoken-rs specifics" in Step 2.
2. **`SecretString` and env interpolation order.** Verified:
   `load_provider_config` (`provider_config.rs:1164`) interpolates
   `${VAR}` in the raw TOML string BEFORE `toml::from_str`
   (`interpolate_env_vars(&raw)` at `:1211`, `toml::from_str(&interpolated)`
   at `:1220`). So `${VAR}` is resolved before serde builds the
   `SecretString`. No extra work needed; the existing interpolation hook
   covers `api_key` because it runs on the whole raw string.
3. **`log_format` env vs. TOML.** Resolved: `RUST_LOG_FORMAT` env var at
   init (via `LogFormat::from_env()`, reading the env var case-
   insensitively, defaulting to `Plain`); TOML `log_format` field is the
   documented default the operator sets so a wrapper script can export
   the env var from it. The subscriber must init before config load
   (existing doc at `state.rs:125-128`), so env-at-init is the only
   viable path.
4. **`Cost` on the wire.** Currently proposed as
   `#[serde(skip_serializing_if = "Option::is_none")]` so it is invisible
   when no pricing is configured. If a future API client wants cost in the
   response body, add an opt-in header like `X-LLM-Proxy-Include-Cost`.
   Decide in the API phase.
