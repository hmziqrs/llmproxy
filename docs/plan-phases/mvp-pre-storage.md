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
   `r50k_base`) and is the de facto Rust port of tiktoken. No custom BPE.
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

## Step 1 — Real upstream message IDs (buffer first event)

### Scope

Only `crates/llm-proxy-server/src/routes/core_pipeline.rs` changes. The
protocol encoders and provider adapters already carry the upstream id;
the route just has to use it.

### Change

In `handle_core_stream` (`core_pipeline.rs:596`), after the upstream byte
stream and `provider_decoder` are ready (`:638-649`), do not construct the
client encoder immediately. Instead:

1. Pull the first SSE frame from the byte stream and decode it with
   `provider_decoder.decode_frame(...)`.
2. Inspect the resulting `Vec<CoreEvent>`:
   - If the first event is `CoreEvent::MessageStart { id: Some(real), .. }`,
     use `real` as `msg_id`.
   - Otherwise (no `MessageStart`, or `id: None`, or an error frame), fall
     back to the existing synthetic id and push the decoded events into a
   small pre-queue that the output stream drains before reading more
   frames.
3. Construct `ClientStreamEncoder::new(client_protocol, msg_id, ...)` with
   the resolved id. Note: the route calls `ClientStreamEncoder::new`
   (`core_pipeline.rs:69`), a wrapper that dispatches to the protocol
   `StreamEncoder::new` for Anthropic or OpenAI. The protocol encoders
   themselves (`client/anthropic.rs:622`, `client/openai_chat.rs:620`)
   are not modified — only the route-level wrapper construction changes.
4. Chain the pre-queue externally before the output stream:
   `futures::stream::iter(pre_queue).chain(output_stream)`. This mirrors
   the existing first-event prepend pattern at `core_pipeline.rs:723-725`.
   Do NOT try to feed the pre-queue into `build_sse_output_stream` — that
   function (`:863`) takes only `byte_stream` + `StreamContext` (which
   owns the decoder/framer/encoder) and has no parameter for a pre-queue.
   External chaining is the correct approach.

The first-byte probe (`first_byte_tx`/`first_byte_rx` at `:684`) already
waits for the first encoded client event before committing HTTP 200. The
buffering happens upstream of that probe, so the probe's semantics are
preserved: errors before the first client byte still become HTTP errors,
errors after still become in-band SSE errors.

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
  events that did arrive.
- The first frame decodes to an `Error` event. The existing
  `FirstByteResult::Error` path handles this; the buffered error event
  flows through it.
- The upstream closes before any frame. Existing empty-stream handling
  applies; no id is needed.

### Why not a pre-flight request

A pre-flight non-streaming request to fetch the id would double the
upstream cost and latency and would not match the id of the streaming
response. Buffering the first event is the standard approach and adds no
upstream traffic.

### Tests

- A mocked upstream whose first event is `MessageStart { id: Some("msg_real_123") }`
  produces client SSE with `id: "msg_real_123"` (Anthropic) or
  `id: "chatcmpl-real_123"` (OpenAI, prefix preserved).
- A mocked upstream with `MessageStart { id: None }` falls back to the
  synthetic `msg_`/`chatcmpl-` id.
- A mocked upstream that opens with `Ping` then `MessageStart` still
  surfaces the real id (the pre-queue drains correctly).
- No events are lost or duplicated when the first frame contains multiple
  events.

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
pub trait Tokenizer: Send + Sync {
    /// Count tokens in a single text string.
    fn count_tokens(&self, text: &str) -> usize;
}

/// Heuristic backend (~4 chars/token). Zero-allocation, infallible.
pub struct HeuristicTokenizer;

/// BPE backend using a specific tiktoken encoding.
pub struct TiktokenTokenizer {
    bpe: tiktoken_rs::CoreBPE,
}
```

`Counter` becomes a small enum or holds an `Arc<dyn Tokenizer>` selected
by model id:

```rust
pub struct Counter {
    default: HeuristicTokenizer,
    // Optional model-specific BPE handle, resolved once at startup or
    // lazily on first use. Kept cheap to clone (Arc).
}
```

The `count_tokens` and `count_messages` methods keep their current
signatures so callers (including `token_count.rs`) are unchanged at the
call site. The methods dispatch to the selected backend.

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

If `tiktoken_rs::o200k_base()` (or any encoding constructor) fails at
runtime (missing ranks file, offline), log a `warn!` once and use the
heuristic for that encoding. The proxy must never fail a request because
the tokenizer could not load; the heuristic is always available.

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

`Decimal` serializes as a string to preserve precision. Document this in
`config.toml.example` and the provider examples under `providers/`.

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
`Usage` is final.

### Integration

- `crates/llm-proxy-core/src/provider_registry.rs` — expose a
  `pricing_for(&self, provider: &str, upstream_model: &str) ->
  Option<&ModelPricing>` accessor on `ProviderRegistry` so the pipeline
  can look up pricing after alias resolution (the upstream model id is
  the key, per locked decision 8).
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
- `CoreResponse` (`core.rs:747`) gains an optional `cost: Option<Cost>`
  field, serialized as `cost` in the normalized core response. The client
  encoders (`client/anthropic.rs`, `client/openai_chat.rs`) are not
  required to surface it on the wire (OpenAI/Anthropic don't have a cost
  field); it is available to the event log and the future API crate.
  Keep `#[serde(skip_serializing_if = "Option::is_none")]` so existing
  wire shapes are byte-identical when no pricing is configured.

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyEvent {
    RequestReceived(RequestReceived),
    ResponseCompleted(ResponseCompleted),
    ResponseFailed(ResponseFailed),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestReceived {
    pub request_id: String,
    pub timestamp: OffsetDateTime,
    pub provider: String,
    pub route_kind: String,
    pub client_protocol: String,
    pub model: ModelRef,
    pub streaming: bool,
    /// SHA-256 of the request body for dedup/correlation (not the body
    /// itself, which may contain secrets).
    pub body_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseCompleted {
    pub request_id: String,
    pub timestamp: OffsetDateTime,
    pub provider: String,
    pub upstream_message_id: Option<String>,
    pub model: ModelRef,
    pub usage: Usage,
    pub cost: Option<Cost>,
    pub stop_reason: StopReason,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFailed {
    pub request_id: String,
    pub timestamp: OffsetDateTime,
    pub provider: String,
    pub model: ModelRef,
    pub error_kind: String,
    /// Sanitized error message; never includes api keys or raw upstream
    /// bodies.
    pub message: String,
    pub http_status: u16,
    pub latency_ms: u64,
}

/// In-process event sink. Implementations: `NoopBus`, `RecordingBus`
/// (tests), and the future SQLite backend.
pub trait EventBus: Send + Sync {
    fn emit(&self, event: ProxyEvent);
}

/// Default no-op sink.
#[derive(Debug, Clone, Default)]
pub struct NoopBus;
impl EventBus for NoopBus {
    fn emit(&self, _event: ProxyEvent) {}
}

/// Test-only sink that records events for assertions.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct RecordingBus {
    pub events: std::sync::Mutex<Vec<ProxyEvent>>,
}
```

### Wiring

`crates/llm-proxy-server/src/state.rs` — add `event_bus: Arc<dyn EventBus>`
to `AppState`, defaulting to `Arc::new(NoopBus)`. `cmd_serve` constructs
the real bus (still `NoopBus` in this plan; the persisted bus lands later).

`crates/llm-proxy-server/src/routes/core_pipeline.rs` — emit:

- `RequestReceived` at the end of `prepare_request` (`:224`), after the
  request id and provider are known. `body_hash` is computed over the
  encoded `CoreRequest` bytes (or the raw client body) with `sha2`; add
  `sha2` to workspace deps. Never include the body itself.
- `ResponseCompleted` at the success tail of `handle_core_once` and at
  the `MessageStop` tail of `build_sse_output_stream`. Includes `usage`,
  `cost` (from Step 3), `upstream_message_id` (from Step 1),
  `stop_reason`, `latency_ms`.
- `ResponseFailed` at every `RouteError` return in the pipeline, with
  `error_kind` from the `RouteError` variant and `http_status` from
  `map_upstream_status` / the route error's status mapping.

The emit calls must never block the hot path. `EventBus::emit` takes
`&self` and is expected to be cheap (queue/spawn). Document that backend
implementations must not do synchronous IO on the calling task; the
SQLite backend will spawn a dedicated writer task.

### Dependencies

- `sha2` is already a direct dep of `llm-proxy-server`
  (`crates/llm-proxy-server/Cargo.toml:31`, version `0.10`, in
  `Cargo.lock` as `0.10.9`). No workspace-deps change needed for sha2;
  it is already available for `body_hash`.
- `time` is in workspace deps (`Cargo.toml:39`) but with features
  `["formatting", "parsing"]` only — **no `serde` feature**. The
  `OffsetDateTime` fields in `ProxyEvent` etc. use
  `#[derive(Serialize, Deserialize)]`, which requires `time`'s `serde`
  feature. **Must change** `Cargo.toml:39` to:
  `time = { version = "=0.3.44", features = ["formatting", "parsing", "serde"] }`.
  Then add `time = { workspace = true }` to `llm-proxy-storage`.
- `llm-proxy-storage` currently has **zero dependencies** (only
  `[package]` + `[lints]`). It now depends on `llm-proxy-protocol` (for
  `Usage`, `Cost`, `ModelRef`, `StopReason`), `rust_decimal`, `time`,
  `secrecy`, and `serde`. No cycle is created: `llm-proxy-protocol` does
  not depend on `llm-proxy-storage`.
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
- `LogFormat` on `ServerConfig`.
- `Tokenizer` trait, `TiktokenTokenizer`, `HeuristicTokenizer`,
  model-to-encoding map in `token/`.
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

- `ProxyEvent`, `RequestReceived`, `ResponseCompleted`, `ResponseFailed`.
- `EventBus` trait, `NoopBus`, `RecordingBus` (test).
- No persistence backend yet (this plan).

`llm-proxy-server`:

- Buffered-first-event upstream id in `core_pipeline.rs`.
- Tokenizer integration in `token_count.rs` (via `Counter`).
- Cost computation at pipeline boundaries.
- `EventBus` in `AppState`; emit calls at pipeline boundaries.
- `body_hash` computation with `sha2`.

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
| Ch. 9 Send/Sync | `EventBus: Send + Sync` so it can live in `Arc` inside `AppState`. `TiktokenTokenizer` must be `Send + Sync` (verify `CoreBPE` is; if not, wrap in a `Mutex`). |

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
  client SSE id.
- `id: None` falls back to synthetic.
- First-frame `Ping` then `MessageStart` still surfaces the real id.
- Multi-event first frame: no events lost or duplicated.

### Token counting

- Model-to-encoding map selects correct encoding for known OpenAI ids.
- Unknown ids fall back to heuristic.
- BPE count for a fixture string matches recorded tiktoken count.
- `count_messages` within tolerance of provider-reported `Usage` on a
  golden fixture.
- BPE load failure falls back to heuristic without panic.
- Existing `counter.rs` heuristic tests stay green.

### Pricing

- `ModelPricing` parses from TOML with string decimals.
- `Cost::from_usage` matches hand-computed fixture.
- Negative price rejected; empty `ModelId` rejected.
- No `pricing` block -> `Cost = None` -> wire response byte-identical.
- Alias resolution: pricing looked up under upstream id, not alias.

### Event log

- `LogFormat::Json` produces JSON log lines.
- `RecordingBus` captures `RequestReceived` + `ResponseCompleted` for a
  successful non-stream request.
- `RecordingBus` captures `ResponseFailed` with correct `http_status`
  for a 404 unknown-provider.
- Stream request emits `ResponseCompleted` with the real
  `upstream_message_id`.
- `body_hash` stable for identical bodies, differs for different bodies.
- Event JSON contains no `api_key` / secret inner values (source-guard).
- `NoopBus` does not block the request path (latency test).

## Verification Commands

Run after every step, and once at the end:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

End-of-plan source-guard scans:

```sh
rg "api_key:\s*String" crates apps       # expect zero matches (all SecretString)
rg "expose_secret" crates apps           # expect matches in transport.rs, discovery.rs, validation
rg "Debug for Provider(Config|AdapterTarget"  # expect zero manual impls (derived now)
rg "pricing|ModelPricing|Cost" crates    # expect matches in core/protocol/server
rg "EventBus|ProxyEvent" crates          # expect matches in storage/server
```

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

1. **BPE load timing.** Eager at startup (fail fast, offline-unsafe) vs.
   lazy on first use (fast startup, first request pays). Recommendation:
   lazy, with a one-time `warn!` on load failure. Confirm `tiktoken-rs`
   ranks are bundled at build time or fetched at runtime; if fetched,
   offline operation needs a cache path.
2. **`SecretString` and env interpolation order.** Verified:
   `load_provider_config` (`provider_config.rs:1164`) interpolates
   `${VAR}` in the raw TOML string BEFORE `toml::from_str`
   (`interpolate_env_vars(&raw)` at `:1211`, `toml::from_str(&interpolated)`
   at `:1220`). So `${VAR}` is resolved before serde builds the
   `SecretString`. No extra work needed; the existing interpolation hook
   covers `api_key` because it runs on the whole raw string.
3. **`log_format` env vs. TOML.** The subscriber must init before config
   load. Resolution: `RUST_LOG_FORMAT` env var at init; TOML field is the
   documented default the operator sets so a wrapper script can export
   the env var from it. Alternative: defer subscriber init until after
   config load and accept that pre-config log lines use the plain format.
   Recommendation: env-at-init (matches `RUST_LOG`).
4. **`Cost` on the wire.** Currently proposed as
   `#[serde(skip_serializing_if = "Option::is_none")]` so it is invisible
   when no pricing is configured. If a future API client wants cost in the
   response body, add an opt-in header like `X-LLM-Proxy-Include-Cost`.
   Decide in the API phase.
