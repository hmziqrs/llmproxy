# Phase 1 - Add Core Protocol Types

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: add normalized chat types without changing live routes.

### Files

Add:

```text
crates/llm-proxy-protocol/src/core.rs
```

Update:

```text
crates/llm-proxy-protocol/src/lib.rs
```

### `core.rs` target shape

Keep these types intentionally chat-endpoint focused. Do not add embeddings,
image generation, audio endpoints, rerank, files, or batch endpoint families in
this phase. Chat content still must preserve image/document/audio/video blocks
because those can appear inside chat requests and responses.

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub requested: String,
    pub upstream: Option<String>,
}

impl ModelRef {
    pub fn provider_model(&self) -> &str {
        self.upstream.as_deref().unwrap_or(&self.requested)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreRequest {
    pub model: ModelRef,
    pub system: Vec<CoreContent>,
    pub messages: Vec<CoreMessage>,
    pub tools: Vec<CoreTool>,
    pub tool_choice: Option<CoreToolChoice>,
    pub sampling: SamplingOptions,
    pub stream: bool,
    pub metadata: RequestMetadata,
    pub provider_hints: ProviderHints,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreMessage {
    pub role: CoreRole,
    pub content: Vec<CoreContent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreContent {
    Text { text: String, cache: Option<CacheControl> },
    Image { source: serde_json::Value },
    Document { source: serde_json::Value },
    Audio { source: serde_json::Value },
    Video { source: serde_json::Value },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult {
        tool_use_id: String,
        content: Vec<CoreContent>,
        is_error: bool,
    },
    Thinking { text: String, signature: Option<String> },
    RedactedThinking { data: serde_json::Value },
    Refusal { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    pub r#type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
    Raw(serde_json::Value),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SamplingOptions {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
    pub stop: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
    pub thinking: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestMetadata {
    pub user_id: Option<String>,
    pub raw: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderHints {
    pub raw: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreResponse {
    pub id: Option<String>,
    pub model: ModelRef,
    pub content: Vec<CoreContent>,
    pub stop_reason: StopReason,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
    pub provider_meta: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    Refusal,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub reasoning_tokens: Option<i32>,
    pub cache_creation_input_tokens: Option<i32>,
    pub cache_read_input_tokens: Option<i32>,
    pub provenance: UsageProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UsageProvenance {
    ProviderReported,
    SyntheticZero,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreEvent {
    MessageStart { id: Option<String>, model: ModelRef },
    ContentStart { index: usize, kind: ContentKind },
    TextDelta { index: usize, text: String },
    ThinkingDelta { index: usize, text: String },
    ToolCallStart { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, args_delta: String },
    ToolCallStop { index: usize },
    UsageDelta { usage: Usage },
    MessageStop { stop_reason: StopReason, stop_sequence: Option<String> },
    Error { error: CoreStreamError },
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentKind {
    Text,
    Thinking,
    ToolUse,
    ToolResult,
    Image,
    Document,
    Audio,
    Video,
    Refusal,
}
```

v1 streaming carries only `Text`, `Thinking`, and `ToolUse` incrementally
(text/thinking/tool-call deltas). `Image`, `Document`, `Audio`, and `Video`
have no per-delta stream event; if a provider streams them, the adapter buffers
the block and emits it through the non-stream `CoreResponse` content path, or
rejects it. Stream encoders therefore never receive deltas for those kinds.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStreamError {
    pub kind: CoreStreamErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreStreamErrorKind {
    InvalidRequest,
    Authentication,
    Permission,
    RateLimit,
    Upstream,
    Internal,
}
```

### Core invariants

- `CoreRequest.system` is the canonical location for system/developer
  instructions. Client adapters should normalize system-role messages into this
  field where the client protocol allows it. `CoreRole::System` exists only for
  protocols or future edge cases that cannot separate system content cleanly.
- `CoreRequest.messages` should contain conversation turns after system content
  has been separated.
- `CoreRequest.model.requested` is always the client-facing model. Provider
  resolution may set `ModelRef.upstream`, but adapters must not replace
  `requested`.
- `CoreToolChoice::Raw` is allowed only as a temporary preservation mechanism
  for a client tool-choice shape that affects behavior but has no canonical
  variant yet. It must not contain unrelated wire nesting, and adapter tests
  must either round-trip it intentionally or move it into metadata/provider
  hints.
- `RequestMetadata.raw`, `ProviderHints.raw`, and `provider_meta` are the only
  places for opaque data. No adapter may special-case another protocol inside
  these core types.
- The stream error type is named `CoreStreamError` (with `CoreStreamErrorKind`),
  not `CoreError`, to avoid colliding with `llm_proxy_core::CoreError`, which is
  the config/registry error owned by the core crate. The two are unrelated types
  in different crates.
- Usage must carry provenance. Never report fabricated or zero-filled usage as
  `ProviderReported`; absent or zero-filled provider usage is
  `SyntheticZero`/`Unknown`. `reasoning_tokens` holds OpenAI/Responses-style
  reasoning token counts when the provider reports them.
- Lossy field drops must be observable, never silent. A dropped request field
  (during provider `encode_request`) must emit a `tracing::warn!`; a dropped
  response field records a warning in `CoreResponse.provider_meta` (e.g. under a
  `warnings` key) in addition to `tracing::warn!`. Silent drops are disallowed.

### Tests

Add unit tests in `core.rs`:

> **Note:** All types derive `PartialEq` (and `Eq` where possible) beyond
> what the derive lists above show.  This is needed for `assert_eq!` in
> round-trip tests and is a positive deviation from the original derive
> lists.

- `model_ref_provider_model_uses_requested_without_override`
- `model_ref_provider_model_uses_upstream_override`
- `sampling_options_default_has_no_overrides`
- `usage_default_is_zero`
- `core_content_tool_result_can_nest_text`
- `core_response_can_preserve_stop_sequence`
- `message_stop_event_can_preserve_stop_sequence`
- `core_request_preserves_client_intent`
- `core_request_preserves_metadata_and_provider_hints`
- `core_request_system_field_is_canonical`
- `core_request_round_trips_through_json`
- `core_response_round_trips_through_json`
- `core_event_round_trips_through_json`
- `core_tool_choice_raw_round_trips_when_intentional`
- `usage_default_provenance_is_unknown`
- `usage_preserves_reasoning_tokens`

Add an integration smoke test in
`crates/llm-proxy-protocol/tests/core_exports.rs` that imports:

> **Note:** The actual integration test imports a broader set of types
> (`CacheControl`, `CacheControlType`, `CoreEvent`, `CoreRequest`,
> `CoreResponse`, `CoreRole`, `CoreMessage`, `CoreContent`, `ModelRef`,
> `RequestMetadata`, `SamplingOptions`, `ProviderHints`) to verify more
> exports are publicly accessible.  This is a superset of the plan's
> minimum requirement.

```rust
use llm_proxy_protocol::core::{CoreEvent, CoreRequest, CoreResponse};
```

This prevents the Phase 1 gate from passing if `core.rs` or `pub mod core;` is
missing.

### Gate

```sh
cargo test -p llm-proxy-protocol core
cargo test -p llm-proxy-protocol --test core_exports
cargo test --workspace
```

### Deviations from original plan

The following intentional deviations were made during implementation and
codified during the audit process:

1. **`CacheControl.type` uses `CacheControlType` enum** instead of raw `String`.
   The enum has `Ephemeral` and `Other(String)` variants with custom
   `Serialize`/`Deserialize` impls.  The JSON wire format is identical, but
   the type system now distinguishes known vs unknown cache control types.
   Also provides `as_str()` for non-consuming access.

2. **`SamplingOptions.stop` uses `Option<Vec<String>>`** with a custom
   deserializer instead of `Option<serde_json::Value>`.  The deserializer
   normalises the OpenAI wire format (bare string `"STOP"`, array `["STOP"]`,
   or null) into `Vec<String>`.  This provides type safety while maintaining
   wire compatibility.

3. **`CoreResponse.provider_meta` uses `serde_json::Map<String, Value>`**
   instead of `serde_json::Value`.  The Map type serializes as `{}` when empty
   rather than `null`, distinguishing "no metadata" from "null metadata".

4. **All types derive `PartialEq`** (and `Eq` where serde_json::Value is not
   present).  This enables `assert_eq!` in round-trip tests.

5. **All public enums use `#[non_exhaustive]`** so adding variants is not a
   semver break.  Structs do not use `#[non_exhaustive]` because adapters
   must be able to construct them with struct literals.

6. **All structs use `#[serde(deny_unknown_fields)]`** to prevent silent
   field drops during deserialization.

7. **All opaque/sensitive fields use redacting `Debug` implementations**
   instead of derived `Debug`.  `serde_json::Value` fields in
   `CoreContent`, `CoreTool`, `CoreToolChoice::Raw`, `RequestMetadata`,
   `ProviderHints`, `CoreResponse.provider_meta`, and
   `SamplingOptions.thinking` show only type and size.  The `Thinking`
   variant's `signature` field is redacted to `[REDACTED]`.
   `CoreStreamError.message` shows only length.

8. **`CoreStreamError` implements `Display` and `std::error::Error`** in
   addition to the plan's `Debug` and `Clone`.

9. **`Usage` has `synthetic_zero()` and `provider_reported()` constructors**
   to help enforce the usage provenance invariant.

### Tracking for future phases

- Lossy-drop warning tests (`tracing::warn!` on dropped fields, `warnings`
  key in `provider_meta`) belong to the adapter phase, not Phase 1, since
  no adapter code exists yet.
- Cancellation / client-disconnect semantics currently map to
  `CoreStreamErrorKind::Internal`.  A dedicated `Cancelled` or
  `ClientDisconnect` variant should be considered in a future phase.
- Token count upper-bound validation (capping at a reasonable maximum) is
  deferred to a future phase.
