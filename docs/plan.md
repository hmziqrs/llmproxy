# Protocol Normalization Implementation Plan

> **Status:** Active implementation plan for the next protocol/provider phase
> **Date:** 2026-06-05
> **Active path:** `docs/plan.md`
> **Standards:** `docs/protocol-mini.md`,
> `docs/protocol-normalization.md`
> **History:** Continues `docs/completed/initial-server-plan.md` and
> supersedes the old `docs/initial-server-plan-continuation.md` path.

## Purpose

The first server plan got the workspace and server running. The current code is
no longer the empty scaffold described at the beginning of that plan. It now has:

- JSON `oc-go-cc`-style config in `crates/llm-proxy-core/src/config.rs`.
- Scenario routing and fallback/circuit-breaker code under
  `crates/llm-proxy-core/src/router/`.
- Anthropic, OpenAI Chat Completions, Responses, and Gemini wire types in
  `crates/llm-proxy-protocol/src/`.
- Direct Anthropic-to-provider transformers in
  `crates/llm-proxy-protocol/src/transformer/`.
- `OpenCodeClient` with model-ID endpoint classification in
  `crates/llm-proxy-provider/src/client.rs`.
- `/v1/messages` wired in `crates/llm-proxy-server/src/routes/messages.rs`.
- An OpenAI echo route file at `crates/llm-proxy-server/src/routes/chat.rs`,
  but it is not mounted in `routes/mod.rs`.
- Metrics, rate limiting, request deduplication, request IDs, token counting,
  daemon/PID handling, and CLI commands already implemented.

The next phase is not "make providers configurable" in isolation. The next
phase is to make the live proxy follow the protocol-normalized architecture:

```text
client wire protocol -> CoreRequest/CoreResponse/CoreEvent -> provider wire protocol
```

Provider configurability lands inside that architecture, not beside it.

This plan uses the local names `CoreRequest`, `CoreResponse`, and `CoreEvent`
for the v1 chat-family core. Those are the same architectural layer called
`CoreChat` and `CoreChatStream` in `docs/protocol-mini.md`; they are not a
generic core for embeddings, images, audio, rerank, files, or batch endpoints.
Future endpoint families must get their own core contracts.

## Hard Rules

These rules prevent the plan from drifting into another direct pairwise
transform design.

1. No direct protocol pairs:

   ```text
   OpenAI Chat -> Anthropic Messages
   Anthropic Messages -> OpenAI Chat
   OpenAI Responses -> Anthropic Messages
   Gemini GenerateContent -> OpenAI Chat
   Gemini GenerateContent -> Anthropic Messages
   ```

   Every conversion must be:

   ```text
   client wire -> chat-family core -> provider wire
   ```

2. The router only selects:

   ```text
   requested model -> provider + upstream model
   ```

   It must not choose protocol families, build URLs, mutate sampling options, or
   inspect message content.

3. Provider adapters own provider wire details:

   - endpoint URL shape
   - request JSON shape
   - response JSON shape
   - streaming chunk parsing
   - provider-specific compatibility behavior

4. Client adapters own client wire details:

   - route request JSON to core
   - core response to route response JSON
   - core event stream to route SSE/chunks
   - route-specific error envelope

5. Client intent is preserved in `CoreRequest`.

   `temperature`, `top_p`, `max_tokens`, tools, tool choice, metadata,
   reasoning/thinking, cache hints, and `stream` come from the client request.
   Provider adapters may translate or omit unsupported fields, but the router and
   config do not override them.

6. A provider can be added with TOML only when it uses an already implemented
   provider protocol adapter. A new wire protocol requires code and fixtures.

7. Each phase must leave the workspace compiling and tests passing.

8. The generic paths in `docs/protocol-normalization.md` map to this repo's
   crate layout:

   ```text
   protocol/<client>.rs -> crates/llm-proxy-protocol/src/client/<client>.rs
   provider/<protocol>.rs -> crates/llm-proxy-provider/src/adapter/<protocol>.rs
   ```

   Only truly new provider wire formats need additional wire DTO modules.

## Target Crate Boundaries

### `llm-proxy-core`

Owns config, routing data, metrics, PID, and token counting.

Keep:

- `error.rs`
- `metrics.rs`
- `pid.rs`
- `token/`

Replace later:

- `config.rs` old JSON `Config`, `ModelConfig`, `OpenCodeGoConfig`,
  `OpenCodeZenConfig`
- `router/` scenario/fallback routing

Add:

- provider/model TOML config types
- provider registry validation
- model routing table lookup

### `llm-proxy-protocol`

Owns wire DTOs and normalized protocol types.

Keep as wire modules:

- `anthropic.rs`
- `openai.rs`
- `zen.rs`

Add:

- `core.rs`
- `client/mod.rs`
- `client/anthropic.rs`
- `client/openai_chat.rs`

Replace later:

- `transformer/request.rs`
- `transformer/response.rs`
- `transformer/stream.rs`

The replacement is not one huge file. It is adapters:

```text
Anthropic Messages <-> CoreRequest/CoreResponse/CoreEvent
OpenAI Chat        <-> CoreRequest/CoreResponse/CoreEvent
```

Provider-side protocol adapters live in `llm-proxy-provider`, but may reuse wire
DTOs from this crate.

### `llm-proxy-provider`

Owns upstream HTTP transport and provider protocol adapters.

Replace:

- `OpenCodeClient`
- `EndpointType`
- `classify_endpoint`
- `is_anthropic_model`
- `is_gemini_model`
- `is_responses_model`
- hardcoded OpenCode endpoint resolution

Add:

- protocol-neutral `ProxyClient`
- provider adapter trait/enum dispatch
- provider adapter registry
- OpenAI Chat Completions provider adapter
- Anthropic Messages provider adapter
- OpenAI Responses provider adapter
- Gemini GenerateContent provider adapter

### `llm-proxy-server`

Owns HTTP routes and application state.

Keep:

- middleware
- shutdown
- token count route initially
- metrics recording
- request ID generation
- rate limiting and request deduplication

Replace:

- `ModelRouter` in `state.rs`
- `fallback_handler` state
- scenario detection in `routes/messages.rs`
- endpoint classification dispatch in `routes/messages.rs`
- route-specific direct transformers
- circuit-breaker output in `routes/health.rs`

Add/mount:

- real `/v1/chat/completions` route using the same core pipeline
- route pipeline helper shared by Anthropic and OpenAI Chat routes

### `apps/llm-proxy`

Owns CLI, process lifecycle, and config file commands.

Replace gradually:

- JSON default config string
- hardcoded model list
- `OC_GO_CC_CONFIG` as the only env var
- validation output that assumes scenarios/fallbacks/OpenCode-only providers

Add:

- TOML config generation
- provider file generation
- provider adapter validation
- model table listing from config

## Target Runtime Pipeline

### Non-Streaming

```text
HTTP route
  -> parse client request JSON
  -> client adapter decode_request(...)
  -> CoreRequest
  -> routing table lookup by CoreRequest.model.requested
  -> ProviderTarget { provider, requested_model, upstream_model }
  -> provider registry resolve adapter by provider-local model table
  -> provider adapter encode_request(...)
  -> ProxyClient send(...)
  -> provider adapter decode_response(...)
  -> CoreResponse
  -> client adapter encode_response(...)
  -> HTTP JSON response
```

### Streaming

```text
HTTP route
  -> parse client request JSON
  -> client adapter decode_request(...)
  -> CoreRequest { stream: true, ... }
  -> routing table lookup
  -> provider registry resolve adapter
  -> provider adapter encode_request(...)
  -> ProxyClient send_stream(...)
  -> provider adapter decode stream into CoreEvent values
  -> client adapter encode CoreEvent values into route SSE/chunks
  -> HTTP stream response
```

Provider stream state stays in provider adapters. Client stream state stays in
client adapters. No route should parse OpenAI chunks and emit Anthropic events
directly.

## Phase 0 - Current-State Guardrails

Goal: document and test what exists before replacing it.

### Files

No production files change in this phase.

Add or update tests only if missing:

- `crates/llm-proxy-protocol/src/transformer/request.rs`
- `crates/llm-proxy-protocol/src/transformer/response.rs`
- `crates/llm-proxy-protocol/src/transformer/stream.rs`
- `crates/llm-proxy-server/tests/chat_echo.rs`

### Required checks

Run:

```sh
cargo test --workspace
cargo clippy --all-targets --all-features --locked -- -D warnings
```

If these fail before Phase 1, fix the current code first. Do not start the
normalization migration on a red baseline.

### Baseline facts to preserve during migration

- `/v1/messages` accepts Anthropic Messages request JSON.
- `/v1/messages/count_tokens` returns an Anthropic-style token estimate.
- `/health`, `/ready`, and `/version` are live.
- `routes/chat.rs` exists but `/v1/chat/completions` is currently not mounted.
- The current transformer tests cover text, system prompts, tool calls, tool
  results, thinking blocks, streaming chunks, stop reasons, and usage mapping.

## Phase 1 - Add Core Protocol Types

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

Keep these types intentionally chat-focused. Do not add embeddings, images,
audio, rerank, files, or batch endpoints in this phase.

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
    pub cache_creation_input_tokens: Option<i32>,
    pub cache_read_input_tokens: Option<i32>,
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
    Error { error: CoreError },
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreError {
    pub kind: CoreErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreErrorKind {
    InvalidRequest,
    Authentication,
    Permission,
    RateLimit,
    Upstream,
    Internal,
}
```

### Tests

Add unit tests in `core.rs`:

- `model_ref_provider_model_uses_requested_without_override`
- `model_ref_provider_model_uses_upstream_override`
- `sampling_options_default_has_no_overrides`
- `usage_default_is_zero`
- `core_content_tool_result_can_nest_text`
- `core_response_can_preserve_stop_sequence`
- `message_stop_event_can_preserve_stop_sequence`

### Gate

```sh
cargo test -p llm-proxy-protocol core
cargo test --workspace
```

## Phase 2 - Add Client Protocol Adapters

Goal: decode client requests into `CoreRequest` and encode core results back to
the client protocol. Do not touch provider code yet.

### Files

Add:

```text
crates/llm-proxy-protocol/src/client/mod.rs
crates/llm-proxy-protocol/src/client/anthropic.rs
crates/llm-proxy-protocol/src/client/openai_chat.rs
```

Update:

```text
crates/llm-proxy-protocol/src/anthropic.rs
crates/llm-proxy-protocol/src/lib.rs
```

### Module exports

```rust
pub mod anthropic;
pub mod client;
pub mod core;
pub mod openai;
pub mod transformer;
pub mod zen;
```

### Adapter error

In `client/mod.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("encode error: {0}")]
    Encode(String),
    #[error("decode error: {0}")]
    Decode(String),
}
```

This requires adding `thiserror = { workspace = true }` to
`crates/llm-proxy-protocol/Cargo.toml`.

### Wire DTO updates

Before writing adapters, make sure the existing wire DTOs can represent the
client fields the core contract preserves. In `anthropic.rs`, add missing fields
instead of dropping them during decode/encode:

```rust
pub struct MessageRequest {
    // existing fields...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

pub struct ContentBlock {
    // existing fields...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

pub struct Delta {
    // existing fields...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}
```

`Delta.stop_sequence` is needed for Anthropic `message_delta` stream events.
`MessageRequest.tool_choice` and text/cache controls are needed because the
core request contract preserves tool choice and cache hints.

`ContentBlock` currently has a custom `Serialize` implementation. Update it so
text blocks emit `cache_control` when present; otherwise the field will decode
but disappear on encode.

### Client adapter functions

Do not force trait abstraction before both adapters exist. Start with plain
module functions:

```text
client::anthropic::decode_request(MessageRequest) -> Result<CoreRequest, ProtocolError>
client::anthropic::encode_response(CoreResponse) -> Result<MessageResponse, ProtocolError>
client::anthropic::encode_event(CoreEvent) -> Result<Vec<MessageEvent>, ProtocolError>

client::openai_chat::decode_request(ChatCompletionRequest) -> Result<CoreRequest, ProtocolError>
client::openai_chat::encode_response(CoreResponse) -> Result<ChatCompletionResponse, ProtocolError>
client::openai_chat::encode_event(CoreEvent) -> Result<Vec<ChatCompletionChunk>, ProtocolError>
```

Only after these are stable should a trait be introduced.

### Anthropic decode rules

Source: current `anthropic.rs` types.

Map:

```text
MessageRequest.model                       -> CoreRequest.model.requested
MessageRequest.system string/array         -> CoreRequest.system
MessageRequest.messages[].role             -> CoreMessage.role
Message.content string                     -> CoreContent::Text
Message.content[].type=text                -> CoreContent::Text
Message.content[].type=image               -> CoreContent::Image
Message.content[].type=tool_use            -> CoreContent::ToolUse
Message.content[].type=tool_result         -> CoreContent::ToolResult
Message.content[].type=thinking            -> CoreContent::Thinking
Text cache_control                         -> CoreContent::Text.cache
MessageRequest.tools                       -> CoreTool
MessageRequest.tool_choice                 -> CoreToolChoice or Raw
MessageRequest.temperature                 -> SamplingOptions.temperature
MessageRequest.top_p                       -> SamplingOptions.top_p
MessageRequest.max_tokens                  -> SamplingOptions.max_tokens
MessageRequest.thinking                    -> SamplingOptions.thinking
MessageRequest.stream.unwrap_or(false)     -> CoreRequest.stream
MessageRequest.metadata.user_id            -> RequestMetadata.user_id
```

Do not run scenario detection. Do not look up providers. Do not infer endpoint
families.

### Anthropic encode rules

Map:

```text
CoreResponse.id.unwrap_or(generated msg id) -> MessageResponse.id
CoreResponse.model.requested                -> MessageResponse.model
CoreContent::Text                           -> ContentBlock type=text
CoreContent::ToolUse                        -> ContentBlock type=tool_use
CoreContent::ToolResult                     -> ContentBlock type=tool_result
CoreContent::Thinking                       -> ContentBlock type=thinking
StopReason::EndTurn                         -> "end_turn"
StopReason::MaxTokens                       -> "max_tokens"
StopReason::ToolUse                         -> "tool_use"
CoreResponse.stop_sequence                  -> MessageResponse.stop_sequence
CoreEvent::MessageStop.stop_sequence        -> final Anthropic stop_sequence delta
Usage                                       -> anthropic::Usage
```

If `CoreResponse.content` is empty, emit one empty text block.

### OpenAI Chat decode rules

Source: current `openai.rs` types.

Map:

```text
ChatCompletionRequest.model                 -> CoreRequest.model.requested
role=system                                 -> CoreRequest.system text
role=user                                   -> CoreMessage User
role=assistant with content                 -> CoreContent::Text
role=assistant with reasoning_content       -> CoreContent::Thinking
role=assistant with tool_calls              -> CoreContent::ToolUse
role=tool with tool_call_id                 -> CoreContent::ToolResult
ChatMessage.cache_control                  -> CoreContent::Text.cache where applicable
tools[].function                            -> CoreTool
tool_choice                                 -> CoreToolChoice or Raw
temperature/top_p/max_tokens/stop           -> SamplingOptions
reasoning_effort/thinking                   -> SamplingOptions
stream.unwrap_or(false)                     -> CoreRequest.stream
stream_options                             -> ProviderHints.raw["stream_options"]
```

### OpenAI Chat encode rules

Map:

```text
CoreResponse.content Text                   -> assistant.content
CoreResponse.content Thinking               -> assistant.reasoning_content
CoreResponse.content ToolUse                -> assistant.tool_calls
StopReason::ToolUse                         -> finish_reason="tool_calls"
StopReason::MaxTokens                       -> finish_reason="length"
other normal stop                           -> finish_reason="stop"
Usage                                       -> UsageInfo
```

### Tests

Add adapter tests, not only wire-type tests:

```text
crates/llm-proxy-protocol/src/client/anthropic.rs
crates/llm-proxy-protocol/src/client/openai_chat.rs
```

Minimum tests:

- plain text request decodes to core
- system prompt decodes to core
- tool call decodes to core
- tool result decodes to core
- thinking decodes to core
- tool choice decodes to core
- cache control decodes to core
- core text response encodes to route response
- core tool use response encodes to route response
- stop reason mapping
- usage mapping
- stop sequence mapping
- streaming text event mapping
- streaming tool event mapping

### Fixture requirement

Add protocol adapter fixtures in this phase. Do not wait until Phase 12 for
basic golden coverage.

```text
crates/llm-proxy-protocol/tests/fixtures/anthropic/
crates/llm-proxy-protocol/tests/fixtures/openai_chat/
```

Each client adapter must have fixture cases for:

- plain text request decode
- system prompt decode
- tool call decode
- tool result decode
- thinking decode where supported
- tool choice decode
- cache control decode
- core response encode
- stop reason and stop sequence encode
- usage encode
- streaming text event encode
- streaming tool event encode

### Gate

```sh
cargo test -p llm-proxy-protocol client
cargo test --workspace
```

## Phase 3 - Add Provider Config And Routing Types

Goal: add TOML provider/model routing alongside the old JSON config. Do not
remove old `Config` yet.

### Files

Add:

```text
crates/llm-proxy-core/src/provider_config.rs
crates/llm-proxy-core/src/model_route.rs
```

Update:

```text
crates/llm-proxy-core/src/lib.rs
crates/llm-proxy-core/Cargo.toml
```

`toml` and `humantime-serde` already exist in workspace/core dependencies.

### Target TOML schema

Main config:

```toml
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"kimi-k2.6" = { provider = "opencode-go" }
"glm-5" = { provider = "opencode-go" }
"gpt-5.4" = { provider = "opencode-zen" }
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }
```

Provider config:

```toml
[provider]
name = "opencode-zen"
api_key = "${OC_GO_CC_API_KEY}"
auth_style = "bearer"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://opencode.ai/zen/v1/responses"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/v1/messages"

[provider.adapters.gemini]
protocol = "gemini_generate_content"
endpoint = "https://opencode.ai/zen/v1/models/{model}:generateContent"

[provider.models]
"gpt-5.4" = { adapter = "responses" }
"claude-sonnet-4-20250514" = { adapter = "anthropic" }
"gemini-3.5-flash" = { adapter = "gemini" }
```

### Provider config examples

These examples preserve the reusable provider-config detail from the earlier
audit. They belong here because Phase 3 is where the config shape becomes real.

#### Mixed OpenCode Go provider

One provider can expose several implemented provider protocols. The
provider-local `[provider.models]` table decides which adapter a resolved
upstream model uses.

```toml
[provider]
name = "opencode-go"
api_key = "${OC_GO_CC_API_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://opencode.ai/zen/go/v1/chat/completions"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/go/v1/messages"

[provider.models]
"kimi-k2.6" = { adapter = "chat" }
"glm-5" = { adapter = "chat" }
"glm-5.1" = { adapter = "chat" }
"qwen3.5-plus" = { adapter = "chat" }
"qwen3.6-plus" = { adapter = "chat" }
"qwen3.7-max" = { adapter = "chat" }
"deepseek-v4-pro" = { adapter = "chat" }
"deepseek-v4-flash" = { adapter = "chat" }
"minimax-m2.5" = { adapter = "anthropic" }
"minimax-m2.7" = { adapter = "anthropic" }
```

#### OpenAI Chat Completions-compatible provider

This is the common "TOML-only provider" case. It works without code changes
only because `openai_chat_completions` is already implemented.

```toml
[provider]
name = "chutes"
api_key = "${CHUTES_API_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://llm.chutes.ai/v1/chat/completions"

[provider.models]
"deepseek-v4" = { adapter = "chat" }
"llama-4" = { adapter = "chat" }
```

Main config route:

```toml
[models]
"deepseek-v4" = { provider = "chutes" }
"llama-4" = { provider = "chutes" }
```

#### Anthropic Messages-compatible provider

Use `auth_style = "both"` for providers that require both `x-api-key` and
`Authorization: Bearer ...`.

```toml
[provider]
name = "anthropic-compatible"
api_key = "${ANTHROPIC_COMPAT_API_KEY}"
auth_style = "both"

[provider.adapters.messages]
protocol = "anthropic_messages"
endpoint = "https://example.com/v1/messages"

[provider.models]
"claude-sonnet-4-20250514" = { adapter = "messages" }
```

Main config route with a client-facing alias:

```toml
[models]
"claude-4" = { provider = "anthropic-compatible", upstream_model = "claude-sonnet-4-20250514" }
```

#### OpenAI Responses-compatible provider

Responses support is a provider adapter, not a route special case.

```toml
[provider]
name = "responses-provider"
api_key = "${RESPONSES_PROVIDER_API_KEY}"
auth_style = "bearer"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://example.com/v1/responses"

[provider.models]
"gpt-5.4" = { adapter = "responses" }
"gpt-5.5" = { adapter = "responses" }
```

#### Gemini GenerateContent provider

URL templates are provider-adapter behavior. The router does not know that the
model appears in the Gemini URL path.

```toml
[provider]
name = "gemini-provider"
api_key = "${GEMINI_PROVIDER_API_KEY}"
auth_style = "bearer"

[provider.adapters.generate]
protocol = "gemini_generate_content"
endpoint = "https://example.com/v1/models/{model}:generateContent"

[provider.models]
"gemini-3.5-flash" = { adapter = "generate" }
```

#### New provider wire protocol

If a provider does not speak one of the built-in protocol names, TOML is not
enough. Add code first:

```text
1. Add provider wire DTOs if the existing wire modules do not fit.
2. Add a provider adapter that converts CoreRequest into provider JSON.
3. Add response and stream decoders back into CoreResponse/CoreEvent.
4. Register the provider protocol name in ProviderAdapterRegistry::builtin().
5. Add provider golden fixtures in the same phase as the new adapter.
6. Add the provider TOML.
```

No client protocol adapter should change when adding a provider.

### Types

In `provider_config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub models: std::collections::HashMap<String, ModelRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: std::net::SocketAddr,
    #[serde(with = "humantime_serde")]
    pub request_timeout: std::time::Duration,
    pub log_level: String,
    pub hot_reload: bool,
    pub server_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderFile {
    pub provider: ProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub api_key: String,
    pub auth_style: AuthStyle,
    pub adapters: std::collections::HashMap<String, ProviderAdapterConfig>,
    pub models: std::collections::HashMap<String, ProviderModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    Bearer,
    XApiKey,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAdapterConfig {
    pub protocol: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModelConfig {
    pub adapter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoute {
    pub provider: String,
    pub upstream_model: Option<String>,
}
```

In `model_route.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTarget {
    pub provider: String,
    pub requested_model: String,
    pub upstream_model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelRouteError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
}

pub fn resolve_model_route(
    routes: &std::collections::HashMap<String, ModelRoute>,
    requested_model: &str,
) -> Result<ProviderTarget, ModelRouteError> {
    let route = routes
        .get(requested_model)
        .ok_or_else(|| ModelRouteError::UnknownModel(requested_model.to_owned()))?;

    Ok(ProviderTarget {
        provider: route.provider.clone(),
        requested_model: requested_model.to_owned(),
        upstream_model: route
            .upstream_model
            .clone()
            .unwrap_or_else(|| requested_model.to_owned()),
    })
}
```

Export the new config/routing types from `llm-proxy-core/src/lib.rs`:

```rust
pub use provider_config::{
    AppConfig, AuthStyle, ModelRoute, ProviderAdapterConfig, ProviderConfig,
    ProviderFile, ProviderModelConfig, ServerConfig,
};
pub use model_route::{ModelRouteError, ProviderTarget, resolve_model_route};
```

### Validation

Core config validation must check only config shape and references it owns:

- every `[models]` provider exists
- every provider-local model points to an existing adapter
- every endpoint is non-empty
- every `${ENV_VAR}` in `api_key` resolves to a non-empty value
- no `ModelRoute` contains endpoint/protocol fields

Do not make `llm-proxy-core` depend on `llm-proxy-provider`. Core may expose a
validation function that accepts a caller-provided list of known protocol names,
but the compiled adapter registry lives in the provider/server composition
layer. The ownership split is:

```text
llm-proxy-core      -> parses TOML, resolves env vars, validates references
llm-proxy-provider  -> owns compiled provider protocol enum/adapters
llm-proxy-server    -> passes provider registry protocol names into core validation
```

Provider protocol validation must therefore happen when the server composes
`ProviderRegistry` with `ProviderAdapterRegistry::builtin()`, not while core is
parsing TOML in isolation.

### Tests

Add tests for:

- main TOML parse
- provider TOML parse
- `${ENV_VAR}` interpolation
- unknown env var fails validation
- unknown provider in route fails validation
- provider-local unknown adapter fails validation
- known protocol passes validation when supplied by caller
- unknown protocol fails validation when not supplied by caller
- upstream model alias resolves correctly
- unknown model returns `ModelRouteError::UnknownModel`

### Gate

```sh
cargo test -p llm-proxy-core provider_config model_route
cargo test --workspace
```

## Phase 4 - Add Protocol-Neutral Transport

Goal: replace OpenCode-specific HTTP transport without changing live routes yet.

### Files

Add:

```text
crates/llm-proxy-provider/src/error.rs
crates/llm-proxy-provider/src/transport.rs
crates/llm-proxy-provider/src/sse.rs
```

Update:

```text
crates/llm-proxy-provider/src/client.rs
crates/llm-proxy-provider/src/lib.rs
```

Do not delete `client.rs` in this phase.

### Provider error

Move `ProviderError` out of the old OpenCode client before new transport and
adapters depend on it:

```text
crates/llm-proxy-provider/src/client.rs -> crates/llm-proxy-provider/src/error.rs
```

Re-export it from `lib.rs`:

```rust
pub mod error;
pub use error::ProviderError;
```

Then update old `client.rs`, new `transport.rs`, and provider adapters to import
`crate::ProviderError`. This prevents Phase 11 from accidentally deleting the
shared provider error when `OpenCodeClient` is removed.

### Types

```rust
#[derive(Debug, Clone)]
pub struct ProxyClient {
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct ProxyRequest {
    pub url: String,
    pub auth: AuthHeaders,
    pub body: Vec<u8>,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct AuthHeaders {
    pub style: llm_proxy_core::AuthStyle,
    pub api_key: String,
}
```

Methods:

```rust
impl ProxyClient {
    pub fn new() -> Self;

    pub async fn send(&self, req: ProxyRequest) -> Result<Vec<u8>, ProviderError>;

    pub async fn send_stream(
        &self,
        req: ProxyRequest,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static>
        >,
        ProviderError,
    >;
}
```

Transport rules:

- Always set `Content-Type: application/json`.
- Set `Accept: text/event-stream` only for streams.
- `AuthStyle::Bearer` sets `Authorization: Bearer <key>`.
- `AuthStyle::XApiKey` sets `x-api-key: <key>`.
- `AuthStyle::Both` sets both headers.
- HTTP status `>= 400` returns `ProviderError::Api { status, body }`.
- The transport does not know protocol names.
- The transport does not serialize typed request structs. It sends bytes.

### SSE framing helper

Add a small buffered SSE parser in `sse.rs`. Do not split raw HTTP byte chunks
with `str::lines()` in route handlers or adapters. Provider streams can split a
single SSE frame across multiple TCP chunks, and some providers emit comments,
event names, ids, or multi-line `data:` fields.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseFramer {
    // buffered bytes and partially parsed frame state
}

impl SseFramer {
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ProviderError>;
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, ProviderError>;
}
```

Framing rules:

- preserve partial frames across byte chunks
- support `\n` and `\r\n`
- ignore comment lines beginning with `:`
- collect repeated `data:` lines with newline separators
- preserve optional `event:` and `id:` fields
- emit a frame only after a blank line or final `finish()`
- treat `data: [DONE]` as a normal terminal frame for adapters to interpret
- return a provider error for invalid UTF-8

### Tests

Use a local axum test server or `wiremock` if added. If avoiding a new
dependency, use axum in a dev-only integration test.

Test:

- bearer header set
- x-api-key header set
- both headers set
- error status returns `ProviderError::Api`
- stream request sets `Accept: text/event-stream`
- SSE framer handles partial frames split across chunks
- SSE framer handles multi-line data fields
- SSE framer preserves `[DONE]`

### Gate

```sh
cargo test -p llm-proxy-provider transport
cargo test --workspace
```

## Phase 5 - Add Provider Protocol Adapters

Goal: provider adapters convert `CoreRequest` to provider request bytes and
provider response bytes/SSE frames back to core.

### Files

Add:

```text
crates/llm-proxy-provider/src/adapter/mod.rs
crates/llm-proxy-provider/src/adapter/openai_chat.rs
crates/llm-proxy-provider/src/adapter/anthropic.rs
crates/llm-proxy-provider/src/adapter/responses.rs
crates/llm-proxy-provider/src/adapter/gemini.rs
```

Update:

```text
crates/llm-proxy-provider/src/lib.rs
crates/llm-proxy-provider/Cargo.toml
```

### Adapter interface

Start with enum dispatch, not `async_trait`. Encoding/decoding is sync.
Transport remains separate.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderProtocol {
    OpenAiChatCompletions,
    AnthropicMessages,
    OpenAiResponses,
    GeminiGenerateContent,
}

impl ProviderProtocol {
    pub fn name(self) -> &'static str;
    pub fn parse(name: &str) -> Option<Self>;
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterTarget {
    pub provider_name: String,
    pub adapter_name: String,
    pub protocol: ProviderProtocol,
    pub endpoint: String,
    pub auth_style: AuthStyle,
    pub api_key: String,
    pub requested_model: String,
    pub upstream_model: String,
}

#[derive(Debug)]
pub enum ProviderAdapter {
    OpenAiChat(openai_chat::OpenAiChatAdapter),
    Anthropic(anthropic::AnthropicAdapter),
    Responses(responses::ResponsesAdapter),
    Gemini(gemini::GeminiAdapter),
}

impl ProviderAdapter {
    pub fn protocol(&self) -> ProviderProtocol;
    pub fn encode_request(
        &self,
        core: &CoreRequest,
        target: &ProviderAdapterTarget,
    ) -> Result<ProxyRequest, ProviderError>;
    pub fn decode_response(
        &self,
        bytes: &[u8],
        target: &ProviderAdapterTarget,
    ) -> Result<CoreResponse, ProviderError>;
    pub fn new_stream_decoder(
        &self,
        target: &ProviderAdapterTarget,
    ) -> Box<dyn ProviderStreamDecoder + Send>;
}

pub trait ProviderStreamDecoder: std::fmt::Debug {
    fn decode_frame(&mut self, frame: &SseFrame) -> Result<Vec<CoreEvent>, ProviderError>;
    fn finish(&mut self) -> Result<Vec<CoreEvent>, ProviderError>;
}
```

The decoder receives already-framed SSE events from `SseFramer`. It must not
parse raw network chunks. Provider-specific adapters decide how to interpret
`event`, `id`, `data`, and `[DONE]`.

### Registry

```rust
#[derive(Debug, Clone)]
pub struct ProviderAdapterRegistry {
    adapters: std::collections::HashMap<ProviderProtocol, ProviderAdapter>,
}

impl ProviderAdapterRegistry {
    pub fn builtin() -> Self;
    pub fn has_protocol_name(&self, protocol: &str) -> bool;
    pub fn get(&self, protocol: ProviderProtocol) -> Option<&ProviderAdapter>;
}
```

### OpenAI Chat provider adapter

Reuses existing logic from `transformer/request.rs` and
`transformer/response.rs`, but splits it:

```text
CoreRequest -> openai::ChatCompletionRequest
openai::ChatCompletionResponse -> CoreResponse
openai::ChatCompletionChunk stream -> CoreEvent stream
```

Important migration from old code:

- Do not take `ModelConfig`.
- Do not override temperature from config.
- Do not override max tokens from config.
- DeepSeek/Kimi provider quirks may be handled here because they are provider
  compatibility rules, but they must be derived from `target.upstream_model` or
  provider hints, not router scenarios.

### Anthropic provider adapter

For upstream Anthropic-compatible providers:

```text
CoreRequest -> anthropic::MessageRequest
anthropic::MessageResponse -> CoreResponse
anthropic::MessageEvent stream -> CoreEvent stream
```

This replaces the current raw pipe behavior in `handle_anthropic_streaming`.
Even if the provider speaks Anthropic, it still goes through core so all client
protocols can use it.

### Responses provider adapter

Reuses existing logic from `transform_to_responses` and
`transform_responses_response`, but through core:

```text
CoreRequest -> zen::ResponsesRequest
zen::ResponsesResponse -> CoreResponse
zen::ResponsesChunk stream -> CoreEvent stream
```

Handle at least:

- `response.output_text.delta`
- `response.function_call_arguments.delta`
- `response.completed`
- `response.failed`
- output message items
- function call output items
- usage

### Gemini provider adapter

Reuses existing Gemini request/response logic, but through core:

```text
CoreRequest -> zen::GeminiRequest
zen::GeminiResponse -> CoreResponse
zen::GeminiStreamChunk stream -> CoreEvent stream
```

The adapter expands URL templates:

```text
{model} -> target.upstream_model
```

The router must not know Gemini puts the model in the path.

### Tests

Each provider adapter needs tests for:

- core text request to provider request
- core system prompt to provider request
- core tool declaration to provider request
- core tool choice to provider request where supported
- core cache control to provider request where supported
- core tool result to provider request
- provider text response to core response
- provider tool call response to core response
- stop reason mapping
- stop sequence mapping where supported
- usage mapping
- streaming text
- streaming tool call
- streaming terminal frame handling
- malformed stream frame behavior
- provider-specific unsupported field behavior

Use the existing transformer tests as a source of expected behavior, but assert
against core values in the middle.

### Fixture requirement

Add provider adapter fixtures in this phase. Phase 12 is only the final
coverage audit, not the first time fixtures appear.

```text
crates/llm-proxy-provider/tests/fixtures/openai_chat/
crates/llm-proxy-provider/tests/fixtures/anthropic/
crates/llm-proxy-provider/tests/fixtures/responses/
crates/llm-proxy-provider/tests/fixtures/gemini/
```

Each provider protocol must include at least:

- core text request to provider request
- provider text response to core response
- tool request/response mapping where supported
- tool choice mapping where supported
- cache control mapping where supported
- stop reason and stop sequence mapping
- usage mapping
- streaming text events
- streaming tool events where supported
- malformed provider response or stream event

### Gate

```sh
cargo test -p llm-proxy-provider adapter
cargo test --workspace
```

## Phase 6 - Build Provider Registry Resolution

Goal: connect TOML provider config to compiled provider adapters.

### Files

Add:

```text
crates/llm-proxy-core/src/provider_registry.rs
```

or keep this in `provider_config.rs` if it stays small.

Update:

```text
crates/llm-proxy-core/src/lib.rs
```

### Registry responsibilities

The registry loads provider TOML files and resolves a route target into a
provider adapter target.

```rust
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: std::collections::HashMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    pub fn load_from_dir(path: impl AsRef<std::path::Path>) -> Result<Self, CoreError>;

    pub fn validate_protocols(
        &self,
        known_protocols: impl IntoIterator<Item = String>,
    ) -> Result<(), CoreError>;

    pub fn resolve_adapter_target(
        &self,
        target: &ProviderTarget,
    ) -> Result<ProviderAdapterTargetConfig, CoreError>;
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterTargetConfig {
    pub provider_name: String,
    pub adapter_name: String,
    pub protocol: String,
    pub endpoint: String,
    pub auth_style: AuthStyle,
    pub api_key: String,
    pub requested_model: String,
    pub upstream_model: String,
}
```

`llm-proxy-provider` can convert `ProviderAdapterTargetConfig` into its own
`ProviderAdapterTarget` by parsing `protocol`.

### Lookup rule

```text
requested_model -> ModelRoute
ModelRoute.upstream_model.unwrap_or(requested_model) -> upstream_model
provider.models[upstream_model] -> adapter_name
provider.adapters[adapter_name] -> protocol + endpoint
```

This lookup allows aliases:

```toml
[models]
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }

[provider.models]
"claude-sonnet-4-20250514" = { adapter = "anthropic" }
```

### Tests

- provider registry loads multiple files
- duplicate provider names fail
- requested model alias resolves through upstream model
- provider-local missing model fails
- provider-local missing adapter fails
- protocol validation fails for unknown protocol

### Gate

```sh
cargo test -p llm-proxy-core provider_registry model_route
cargo test --workspace
```

## Phase 7 - Rewrite AppState

Goal: state carries the new config/registry/pipeline dependencies while old
routes can still compile until Phase 8.

### Files

Update:

```text
crates/llm-proxy-server/src/state.rs
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
is gone. Remove `LegacyState` after Phase 8 and Phase 9 are complete and CLI
JSON compatibility is no longer needed. Do not leave compatibility fields in
the final state.

Valid Phase 7 construction modes:

- JSON compatibility mode: `legacy = Some(...)`, `app_config = None`,
  `providers = None`. Only old routes should use this mode.
- TOML new-runtime mode: `app_config = Some(...)`, `providers = Some(...)`.
  This is the mode Phase 8 and later route tests must use.

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

### Gate

```sh
cargo test -p llm-proxy-server
cargo test --workspace
```

## Phase 8 - Rewrite `/v1/messages`

Goal: Anthropic client route uses the core pipeline for both streaming and
non-streaming.

### Files

Rewrite:

```text
crates/llm-proxy-server/src/routes/messages.rs
```

Update:

```text
crates/llm-proxy-server/src/routes/token_count.rs
```

Add or update shared pipeline modules:

```text
crates/llm-proxy-server/src/routes/core_pipeline.rs
crates/llm-proxy-server/src/routes/error_response.rs
```

Do not keep:

- `detect_scenario_from_request`
- `build_scenario_config`
- `handle_non_streaming` with fallback chain
- `execute_non_streaming_request` with `classify_endpoint`
- provider-specific `handle_openai_streaming`
- provider-specific `handle_responses_streaming`
- provider-specific `handle_gemini_streaming`
- raw Anthropic streaming pipe
- `spawn_proxy_task` that converts provider chunks directly to Anthropic SSE

### New non-streaming handler outline

```rust
pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match handle_messages_inner(state, headers, body).await {
        Ok(response) => response,
        Err(error) => route_error_response(ClientProtocol::Anthropic, error),
    }
}

async fn handle_messages_inner(
    state: AppState,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    let ctx = prepare_request(&state, &headers, &body)?;
    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;
    req.validate().map_err(RouteError::InvalidRequest)?;

    let core = llm_proxy_protocol::client::anthropic::decode_request(req)
        .map_err(protocol_error_to_route)?;

    if core.stream {
        handle_core_stream(state, ctx, core, ClientProtocol::Anthropic).await
    } else {
        handle_core_once(state, ctx, core, ClientProtocol::Anthropic).await
    }
}
```

Shared `handle_core_once`:

```text
CoreRequest
  -> state.app_config().ok_or_else(|| RouteError::Internal("TOML config required".to_owned()))
  -> state.providers().ok_or_else(|| RouteError::Internal("provider registry required".to_owned()))
  -> resolve_model_route(app_config.models, core.model.requested)
  -> map ModelRouteError::UnknownModel to RouteError::UnknownModel
  -> providers.resolve_adapter_target(...)
  -> ProviderProtocol::parse(...)
  -> state.provider_adapters.get(...)
  -> adapter.encode_request(...)
  -> state.proxy_client.send(...)
  -> adapter.decode_response(...)
  -> anthropic::encode_response(...)
  -> HTTP response
```

Shared `handle_core_stream`:

```text
CoreRequest
  -> state.app_config().ok_or_else(|| RouteError::Internal("TOML config required".to_owned()))
  -> state.providers().ok_or_else(|| RouteError::Internal("provider registry required".to_owned()))
  -> resolve_model_route(app_config.models, core.model.requested)
  -> map ModelRouteError::UnknownModel to RouteError::UnknownModel
  -> providers.resolve_adapter_target(...)
  -> ProviderProtocol::parse(...)
  -> state.provider_adapters.get(...)
  -> adapter.encode_request(...)
  -> state.proxy_client.send_stream(...)
  -> SseFramer.push_chunk(...)
  -> adapter.decode_frame(...)
  -> client adapter encode_event(...)
  -> route-specific SSE/chunk response
  -> adapter.finish()
  -> client adapter terminal event if needed
```

Do not decode provider streams in `/v1/messages` directly. The route chooses the
client protocol encoder only; provider stream parsing belongs to
`SseFramer + ProviderStreamDecoder`.

### Shared route errors

Do not keep an Anthropic-only `ApiError` as the shared pipeline error. Use one
internal error model and encode it per client route.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProtocol {
    Anthropic,
    OpenAiChat,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("upstream error: {status}")]
    Upstream { status: StatusCode, body: String },
    #[error("provider decode error: {0}")]
    ProviderDecode(String),
    #[error("internal error: {0}")]
    Internal(String),
}

pub fn route_error_response(protocol: ClientProtocol, error: RouteError) -> Response<Body>;
```

`/v1/messages` passes `ClientProtocol::Anthropic`. `/v1/chat/completions`
passes `ClientProtocol::OpenAiChat`. The shared pipeline may return
`RouteError`, but only `error_response.rs` knows the client-specific JSON
envelope.

### Token count endpoint

Update `/v1/messages/count_tokens` in this phase so it no longer depends on old
core request helper types that Phase 11 will delete. It remains an Anthropic
client endpoint, but it should parse `MessageRequest`, validate it, decode it
through `client::anthropic::decode_request`, and estimate text tokens from
`CoreRequest.system` and `CoreRequest.messages`.

Do not resolve providers or call upstream for token counting. The endpoint is a
local estimate only.

### Error behavior

- Unknown model: `400 Bad Request`.
- Config/registry resolution error: `500 Internal Server Error` unless it is
  clearly a request model error.
- Upstream `>= 400`: `502 Bad Gateway` with route-specific error envelope.
- Provider adapter decode failure: `502 Bad Gateway`.
- Client adapter decode failure: `400 Bad Request`.

For `/v1/messages`, errors remain Anthropic-shaped.

### Metrics behavior

Keep:

- `metrics.record_request(core.stream)`
- `metrics.record_success(target.upstream_model, latency)`
- `metrics.record_failure()`
- `metrics.record_rate_limited()`
- `metrics.record_deduplicated()`

### Tests

Add server tests with local/mock provider endpoint:

- unknown model returns 400 and does not call upstream
- configured OpenAI Chat provider returns Anthropic response
- configured Responses provider returns Anthropic response
- configured Gemini provider returns Anthropic response
- configured Anthropic provider returns Anthropic response through core, not raw pipe
- upstream 500 returns 502
- invalid JSON returns 400
- request ID header is present on success
- token count endpoint still returns Anthropic-compatible count response
- token count endpoint does not require legacy state

### Gate

```sh
cargo test -p llm-proxy-server messages
cargo test --workspace
```

## Phase 9 - Mount Real `/v1/chat/completions`

Goal: OpenAI Chat client route uses the same core pipeline.

### Files

Rewrite:

```text
crates/llm-proxy-server/src/routes/chat.rs
```

Update:

```text
crates/llm-proxy-server/src/routes/mod.rs
```

Mount:

```rust
.route("/v1/chat/completions", post(handle_chat_completions))
```

Remove or rename the old echo handler. The route should no longer reject
`stream: true`.

### Handler outline

```text
ChatCompletionRequest
  -> client::openai_chat::decode_request
  -> shared core pipeline
  -> client::openai_chat::encode_response
  -> ChatCompletionResponse
```

Reuse `routes/core_pipeline.rs` and `routes/error_response.rs` from Phase 8.
Do not add a second provider execution path in `routes/chat.rs`.

Streaming:

```text
CoreEvent stream
  -> client::openai_chat::encode_event
  -> OpenAI chat completion chunks
  -> final data: [DONE]
```

### Error behavior

For `/v1/chat/completions`, errors must be OpenAI-shaped:

```json
{
  "error": {
    "message": "...",
    "type": "invalid_request_error",
    "code": null
  }
}
```

Do not use Anthropic errors on OpenAI routes.

### Tests

- route is mounted, no longer 404
- non-streaming text request returns OpenAI-shaped response
- tool-call core response returns `tool_calls`
- unknown model returns OpenAI-shaped 400
- upstream failure returns OpenAI-shaped 502
- streaming response emits `chat.completion.chunk`
- streaming response ends with `[DONE]`

### Gate

```sh
cargo test -p llm-proxy-server chat
cargo test --workspace
```

## Phase 10 - Replace CLI Config Commands

Goal: CLI generates and validates the new TOML/provider config.

### Files

Update:

```text
apps/llm-proxy/src/main.rs
```

Add examples:

```text
config.toml.example
providers/opencode-go.toml.example
providers/opencode-zen.toml.example
```

or generate them directly from `llm-proxy init`.

### CLI changes

`serve`:

- `--config` points to `config.toml`.
- `$LLM_PROXY_CONFIG` is the preferred env var.
- `$OC_GO_CC_CONFIG` may be detected temporarily only to print the migration
  warning; do not use it as the preferred live config path.

`init`:

- creates `config.toml`
- creates `providers/opencode-go.toml`
- creates `providers/opencode-zen.toml`
- does not write fallback/scenario JSON

`validate`:

- loads main TOML
- loads provider files
- validates provider adapter protocols against builtins
- prints model route table:

  ```text
  client_model -> provider/upstream_model/adapter/protocol
  ```

`models`:

- lists configured client models from `[models]`
- optionally accepts `--provider` later, but not required in this phase

### Backward compatibility

Phase 10 is the config cutover point. Start this phase only after:

- `/v1/messages` uses the core pipeline
- `/v1/chat/completions` uses the core pipeline
- `/v1/messages/count_tokens` no longer depends on legacy state
- all Phase 8 and Phase 9 tests pass

For one release window, allow JSON config only to print a migration error:

```text
JSON oc-go-cc config is no longer supported by serve.
Run `llm-proxy init` to create TOML config, then copy model/API settings.
```

`serve --config old.json` must exit non-zero before constructing `AppState`.
It must not start the server with old JSON config. If `$OC_GO_CC_CONFIG` is
present, print the same migration error unless an explicit TOML `--config` or
`$LLM_PROXY_CONFIG` is provided.

Do not silently translate old scenario JSON into new model routes. That would
preserve the wrong mental model.

After this phase, remove `LegacyState` from `AppState` unless a still-mounted
route has a documented compile-time dependency on it. The expected result is no
legacy state.

Also make the new runtime fields non-optional:

```rust
pub app_config: Arc<AppConfig>,
pub providers: Arc<ProviderRegistry>,
```

Remove `app_config()` and `providers()` option helpers if they only existed to
bridge Phase 7 JSON compatibility. After Phase 10, serving requires TOML config.

### Tests

If CLI tests are not present, add unit tests for pure helpers:

- config path resolution prefers CLI path
- config path resolution supports `$LLM_PROXY_CONFIG`
- provider directory path is next to config file
- generated TOML parses as `AppConfig`
- generated provider TOML parses as `ProviderFile`

### Gate

```sh
cargo test -p llm-proxy
cargo test --workspace
```

## Phase 11 - Remove Old Direct Architecture

Goal: delete obsolete code only after both live routes use the core pipeline.

### Delete

```text
crates/llm-proxy-core/src/router/
```

Remove exports from:

```text
crates/llm-proxy-core/src/lib.rs
```

Delete or empty:

```text
crates/llm-proxy-protocol/src/transformer/request.rs
crates/llm-proxy-protocol/src/transformer/response.rs
crates/llm-proxy-protocol/src/transformer/stream.rs
```

Prefer deleting `transformer/` entirely once adapters cover all tests.

Remove protocol exports and dependencies tied to the old direct architecture:

```text
crates/llm-proxy-protocol/src/lib.rs
crates/llm-proxy-protocol/Cargo.toml
```

Required cleanup:

- remove `pub mod transformer`
- remove `llm-proxy-core` from `llm-proxy-protocol` dependencies
- keep protocol crate independent of core crate; normalized core types now live
  inside `llm-proxy-protocol::core`
- verify no protocol module imports `llm_proxy_core::*`

Delete from provider:

```text
OpenCodeClient
EndpointType
classify_endpoint
is_anthropic_model
is_gemini_model
is_responses_model
is_zen
provider
```

Delete old config structs:

```text
Config
ModelConfig
OpenCodeGoConfig
OpenCodeZenConfig
LoggingConfig
```

Only delete `LoggingConfig` if replacement `ServerConfig.log_level` is live and
all code uses it.

Remove from server:

```text
ModelRouter
fallback_handler
circuit_breakers in health response
scenario logs
fallback-chain logic
LegacyState
```

### Tests to delete or rewrite

Delete tests that assert scenario routing, fallback, and endpoint
classification.

Rewrite tests that assert useful behavior through the new architecture:

- scenario test for `glm-5.1` becomes route table lookup test
- endpoint classification test becomes provider model adapter lookup test
- stream proxy test becomes provider event decoder + client event encoder tests

### Gate

```sh
cargo test --workspace
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```

## Phase 12 - Complete Golden Fixture Coverage

Goal: audit and complete fixture coverage. Basic client fixtures must already
exist from Phase 2, and basic provider fixtures must already exist from Phase 5.
This phase closes gaps; it must not be the first time fixtures are added.

### Fixture layout

Add:

```text
crates/llm-proxy-protocol/tests/fixtures/anthropic/
crates/llm-proxy-protocol/tests/fixtures/openai_chat/
crates/llm-proxy-provider/tests/fixtures/openai_chat/
crates/llm-proxy-provider/tests/fixtures/anthropic/
crates/llm-proxy-provider/tests/fixtures/responses/
crates/llm-proxy-provider/tests/fixtures/gemini/
```

If these directories already exist from earlier phases, keep them and add only
the missing cases.

Each fixture case should have:

```text
input.json
core.json
output.json
```

For streams:

```text
input.sse
core-events.json
output.sse
```

### Minimum fixture cases

For every adapter:

- plain text
- system prompt
- multiple messages
- tool definition
- tool choice
- assistant tool call
- user tool result
- reasoning/thinking
- cache marker where supported
- stop reason mapping
- stop sequence mapping where supported
- usage mapping
- streaming text
- streaming tool call
- malformed/unsupported provider field

### Dependency choice

Use plain JSON fixture comparison first. Add `insta` only if fixture updates
become tedious.

If adding `insta`, add it as a dev dependency only:

```toml
[dev-dependencies]
insta = { version = "1", features = ["json"] }
```

### Gate

```sh
cargo test --workspace
```

## Final Verification Gate

Run in order:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace
cargo build --workspace --locked --release
```

Manual smoke after `llm-proxy serve --config ./config.toml`:

```sh
curl -sS http://127.0.0.1:3456/health
curl -sS http://127.0.0.1:3456/ready
curl -sS http://127.0.0.1:3456/version
curl -sS -X POST http://127.0.0.1:3456/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"unknown","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
curl -sS -X POST http://127.0.0.1:3456/v1/messages/count_tokens \
  -H 'content-type: application/json' \
  -d '{"model":"any","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
curl -sS -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"unknown","messages":[{"role":"user","content":"hi"}]}'
llm-proxy validate --config ./config.toml
llm-proxy models --config ./config.toml
```

Then stop the TOML server and run the old-JSON migration check as a separate
invocation:

```sh
llm-proxy serve --config ./old.json
```

Expected:

- health/ready/version return 200
- unknown Anthropic model returns 400 Anthropic-shaped error
- token count returns 200 and does not call upstream
- unknown OpenAI Chat model returns 400 OpenAI-shaped error
- validate succeeds for TOML config
- models prints configured client model IDs
- old JSON serve exits non-zero with migration error
- no request performs scenario detection
- no request invokes fallback
- no model ID classifier decides protocol

## Implementation Order Summary

```text
0. Verify current green baseline.
1. Add CoreRequest/CoreResponse/CoreEvent.
2. Add Anthropic and OpenAI Chat client adapters plus client fixtures.
3. Add TOML model/provider config beside old JSON config.
4. Add protocol-neutral ProxyClient and SSE framer.
5. Add provider protocol adapters plus provider fixtures.
6. Add provider registry resolution.
7. Rewrite AppState.
8. Rewrite /v1/messages through core.
9. Mount real /v1/chat/completions through core.
10. Replace CLI config commands.
11. Delete old scenario/fallback/direct-transform code.
12. Complete golden fixture coverage.
```

The runtime architecture is complete only when Phase 11 is done. The migration
is not implementation-complete until Phase 12 verifies fixture coverage. Before
Phase 11, the workspace may contain compatibility code, but new route behavior
must use the core pipeline as soon as Phase 8 starts.
