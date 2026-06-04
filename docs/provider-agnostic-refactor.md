# Provider-Agnostic Refactor

> **Status:** Revised direction - align with protocol normalization
> **Date:** 2026-06-04

## Why

The proxy currently carries the shape of Go `oc-go-cc`: two hardcoded
OpenCode providers, model/scenario classification, direct protocol-pair
transforms, fallback handling, and provider decisions mixed into request
translation.

This refactor makes the proxy provider-agnostic without making protocol
handling ad hoc:

- The client's `model` field drives routing; no scenario detection.
- Routing selects a provider and upstream model only.
- Protocol conversion always goes through the normalized core types from
  `docs/protocol-normalization.md`.
- Provider configs describe auth, endpoints, and supported adapters.
- No fallback, no circuit breaker, no retry in this cut; upstream errors are
  forwarded as proxy errors.
- Client options such as temperature, thinking, reasoning effort, max tokens,
  tools, and streaming are preserved in `CoreRequest`; the server does not
  override them.

The core rule is:

```text
client wire protocol -> CoreRequest/CoreEvent/CoreResponse -> provider wire protocol
```

The proxy must not implement direct pairs such as:

```text
Anthropic Messages -> OpenAI Responses
OpenAI Chat -> Anthropic Messages
Gemini -> OpenAI Chat
```

Those become isolated adapters:

```text
Anthropic Messages -> CoreChat -> OpenAI Responses
OpenAI Chat        -> CoreChat -> Anthropic Messages
Gemini             -> CoreChat -> OpenAI/Anthropic
```

## Config Schema (TOML)

### Main config: `config.toml`

```toml
# llm-proxy main configuration.
#
# Client routes may expose several protocol families, for example
# /v1/messages and /v1/chat/completions. Model routing is protocol-neutral:
# it chooses a provider target, not a wire-format transform.

[server]
bind = "127.0.0.1:3456"     # host:port to listen on
timeout = "300s"            # per-request timeout
log_level = "info"          # trace | debug | info | warn | error
hot_reload = false          # watch config for changes (NYI)
server_name = "llm-proxy"   # reported by /version

# Model routing table.
# Key = model ID the client requests.
# Value = provider target plus optional upstream model ID.
#
# No endpoint/protocol field belongs here. The router does not translate
# protocol fields; provider adapters do.
[models]
"kimi-k2.6"       = { provider = "opencode-go" }
"glm-5"           = { provider = "opencode-go" }
"glm-5.1"         = { provider = "opencode-go" }
"qwen3.5-plus"    = { provider = "opencode-go" }
"qwen3.6-plus"    = { provider = "opencode-go" }
"qwen3.7-max"     = { provider = "opencode-go" }
"deepseek-v4-pro" = { provider = "opencode-go" }
"deepseek-v4-flash" = { provider = "opencode-go" }
"minimax-m2.5"    = { provider = "opencode-go" }
"minimax-m2.7"    = { provider = "opencode-go" }

"gpt-5.4" = { provider = "opencode-zen" }
"gpt-5.5" = { provider = "opencode-zen" }
"gemini-3.5-flash" = { provider = "opencode-zen" }
"claude-sonnet-4-20250514" = { provider = "opencode-zen" }

# Client alias; provider receives the upstream model name.
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }
```

### Provider config: `providers/opencode-go.toml`

```toml
# OpenCode Go provider.
# Serves: kimi, glm, qwen, deepseek, minimax models.

[provider]
name = "opencode-go"
api_key = "${OC_GO_CC_API_KEY}"
auth_style = "bearer"   # bearer | x-api-key | both

# Adapter definitions name implemented provider-protocol adapters.
# `protocol` is validated against the provider adapter registry at startup.
[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://opencode.ai/zen/go/v1/chat/completions"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/go/v1/messages"

# Provider-local model support. The provider registry looks up the resolved
# upstream model here after the router has selected this provider.
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

### Provider config: `providers/opencode-zen.toml`

```toml
# OpenCode Zen provider.
# Serves: gpt, gemini, claude models.

[provider]
name = "opencode-zen"
api_key = "${OC_GO_CC_API_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://opencode.ai/zen/v1/chat/completions"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/v1/messages"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://opencode.ai/zen/v1/responses"

[provider.adapters.gemini]
protocol = "gemini_generate_content"
endpoint = "https://opencode.ai/zen/v1/models/{model}:generateContent"

[provider.models]
"gpt-5.4" = { adapter = "responses" }
"gpt-5.5" = { adapter = "responses" }
"gemini-3.5-flash" = { adapter = "gemini" }
"claude-sonnet-4-20250514" = { adapter = "anthropic" }
```

### Adding providers

There are two different cases.

#### Case 1: Provider uses an implemented protocol

If a new provider speaks an already implemented provider protocol, adding it is
configuration-only.

Example: a new OpenAI Chat Completions-compatible provider:

```toml
# providers/chutes.toml
[provider]
name = "chutes"
api_key = "${CHUTES_API_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://llm.chutes.ai/v1/chat/completions"

[provider.models]
"deepseek-v4" = { adapter = "chat" }
```

```toml
# config.toml
[models]
"deepseek-v4" = { provider = "chutes" }
```

No code changes are required because the `openai_chat_completions` provider
adapter already exists.

#### Case 2: Provider uses a new wire protocol

If a provider has a new or incompatible protocol, TOML is not enough. Add a
provider adapter:

```text
1. Add provider wire types if needed.
2. Convert CoreRequest into provider JSON.
3. Convert provider JSON/SSE into CoreResponse/CoreEvent.
4. Register the adapter protocol name.
5. Add golden fixtures.
6. Add provider TOML.
```

No client protocol adapter should change when adding a provider.

## Architecture

### Layer responsibilities

```text
Route handler
  - selects inbound client protocol by HTTP route
  - invokes the inbound protocol adapter
  - returns the client protocol adapter's encoded response/SSE

Inbound protocol adapter
  - client request JSON -> CoreRequest
  - CoreResponse/CoreEvent -> client response JSON/SSE

Router
  - CoreRequest.model -> ProviderTarget { provider, upstream_model }
  - no protocol translation
  - no endpoint-family classification

Provider registry
  - loads providers/*.toml
  - resolves ProviderTarget into ProviderAdapterTarget
  - validates configured protocol names have compiled adapters

Provider adapter
  - CoreRequest -> provider request JSON
  - provider response JSON/SSE -> CoreResponse/CoreEvent
  - owns provider-specific stream buffering and URL templating

Proxy client
  - HTTP transport, auth headers, timeouts
  - no protocol knowledge beyond body bytes/SSE bytes
```

### Core protocol types

The normalized core lives in `llm-proxy-protocol`, not in the router.

```rust
// crates/llm-proxy-protocol/src/core.rs

CoreRequest {
    model: ModelRef,
    messages: Vec<CoreMessage>,
    system: Vec<CoreContent>,
    tools: Vec<CoreTool>,
    tool_choice: CoreToolChoice,
    sampling: SamplingOptions,
    stream: bool,
    metadata: RequestMetadata,
    provider_hints: ProviderHints,
}

CoreResponse {
    id: Option<String>,
    model: ModelRef,
    content: Vec<CoreContent>,
    stop_reason: StopReason,
    usage: Usage,
    provider_meta: serde_json::Value,
}

CoreEvent {
    MessageStart { id: Option<String>, model: ModelRef },
    ContentStart { index: usize, kind: ContentKind },
    TextDelta { index: usize, text: String },
    ToolCallStart { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, args_delta: String },
    ToolCallStop { index: usize },
    ThinkingDelta { index: usize, text: String },
    UsageDelta { usage: Usage },
    MessageStop { stop_reason: StopReason },
    Error { error: CoreError },
    Ping,
}
```

Core content must support at least:

```text
Text
Image
Document
Audio
Video
ToolUse
ToolResult
Thinking
RedactedThinking
Refusal
```

### Config and registry types

```rust
// crates/llm-proxy-core/src/config.rs

ServerConfig {
    bind: String,
    timeout: Duration,
    log_level: String,
    hot_reload: bool,
    server_name: String,
}

AuthStyle { Bearer, XApiKey, Both }

ProviderConfig {
    name: String,
    api_key: String,
    auth_style: AuthStyle,
    adapters: HashMap<String, ProviderAdapterConfig>,
    models: HashMap<String, ProviderModelConfig>,
}

ProviderAdapterConfig {
    protocol: String,
    endpoint: String,
}

ProviderModelConfig {
    adapter: String,
}

ModelRoute {
    provider: String,
    upstream_model: Option<String>,
}

ProviderTarget {
    provider: String,
    requested_model: String,
    upstream_model: String,
}

ProviderAdapterTarget {
    provider: ProviderConfig,
    adapter: ProviderAdapterConfig,
    requested_model: String,
    upstream_model: String,
}

RoutingTable     = HashMap<String, ModelRoute>
ProviderRegistry = { load_from_dir, resolve_target }
AppConfig        = { server, routing_table, providers }
```

`ModelRoute` intentionally has no `endpoint`, `protocol`, or `adapter` field.
That keeps the router from becoming a protocol translator. The provider
registry uses the upstream model to choose a provider-local adapter, and the
adapter still owns the wire-format conversion.

Initial provider protocol names:

```text
openai_chat_completions
anthropic_messages
openai_responses
gemini_generate_content
```

Those names are config values, not core enum variants. Startup validation checks
that each configured name exists in the compiled provider adapter registry.

### Request flow: non-streaming

```text
Client: POST /v1/messages { model: "gpt-5.4", messages: [...] }
  |
  | route selects Anthropic Messages inbound adapter
  v
AnthropicMessagesClientAdapter.decode_request(json)
  -> CoreRequest { model: "gpt-5.4", ... }
  |
  | router selects only provider/model
  v
routing_table["gpt-5.4"]
  -> ProviderTarget {
       provider: "opencode-zen",
       requested_model: "gpt-5.4",
       upstream_model: "gpt-5.4",
     }
  |
  | provider registry resolves provider-local adapter by upstream model
  v
providers["opencode-zen"].models["gpt-5.4"]
  -> adapter "responses"
  -> protocol "openai_responses"
  |
  | provider adapter converts from CoreRequest
  v
OpenAiResponsesProviderAdapter.encode_request(core, target)
  -> provider JSON
  |
  | transport sends bytes
  v
ProxyClient.send(endpoint, auth, body)
  -> provider JSON response
  |
  | provider adapter converts back to CoreResponse
  v
OpenAiResponsesProviderAdapter.decode_response(json)
  -> CoreResponse
  |
  | inbound client adapter encodes the client's expected shape
  v
AnthropicMessagesClientAdapter.encode_response(core)
  -> MessageResponse JSON
```

Unknown model returns `400 Bad Request`. Upstream failure returns
`502 Bad Gateway`. No retry or fallback happens in this refactor.

### Request flow: streaming

Streaming uses the same route and target resolution, but the response path is
event-based:

```text
provider SSE/chunks
  -> ProviderAdapter.decode_event_stream(...)
  -> Stream<Item = CoreEvent>
  -> ClientProtocolAdapter.encode_event_stream(...)
  -> client SSE/chunks
```

Provider stream parsers own their local state:

- partial tool-call JSON buffers
- provider-specific content indexes
- Anthropic thinking signatures
- usage and stop-reason accumulation
- unknown provider event tolerance
- upstream disconnect to client error mapping

Streaming must not be normalized by concatenating text and pretending the
request was non-streaming.

### Auth styles

| Style | Headers set |
|-------|-------------|
| `bearer` (default) | `Authorization: Bearer <key>` |
| `x-api-key` | `x-api-key: <key>` |
| `both` | Both headers |

Auth is provider config, not protocol logic. If one upstream requires different
auth per endpoint, split it into separate provider configs.

### Model ID override

`upstream_model` lets the proxy expose one name to clients while sending a
different name upstream:

```toml
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }
```

Client sends `model: "claude-4"`. The router resolves the upstream model to
`claude-sonnet-4-20250514`. Adapters should see both the requested model and
the upstream model when they need to preserve client-facing response names.

### URL templating

URL templating is a provider-adapter concern. For example, Gemini may use:

```toml
[provider.adapters.gemini]
protocol = "gemini_generate_content"
endpoint = "https://opencode.ai/zen/v1/models/{model}:generateContent"
```

The Gemini provider adapter expands `{model}` with the upstream model and
serializes `CoreRequest` as Gemini `GenerateContent`. The router does not know
that Gemini places the model in the path.

## What Changes

### Keep

- `crates/llm-proxy-protocol/src/{anthropic,openai,zen}.rs` as wire-type
  modules where they remain accurate.
- `crates/llm-proxy-core/src/{metrics,pid,error}.rs`.
- `crates/llm-proxy-core/src/token/` heuristic counter.
- `crates/llm-proxy-server/src/{middleware,shutdown,error}.rs`.
- `crates/llm-proxy-server/src/routes/token_count.rs`.

### Delete

- `crates/llm-proxy-core/src/router/` scenario and fallback machinery.
- Model-ID endpoint classifiers such as `is_anthropic_model` and
  `classify_endpoint`.
- `OpenCodeClient`, `ModelRouter`, and `FallbackHandler` once replacements are
  live.
- Direct protocol-pair transformer entry points after adapters cover them.
- Old provider-specific config structs such as `OpenCodeGoConfig` and
  `OpenCodeZenConfig`.

### Rewrite / Add

| File | Change |
|------|--------|
| `crates/llm-proxy-core/src/config.rs` | TOML config types: `AppConfig`, `ProviderConfig`, `ProviderAdapterConfig`, `ModelRoute`, registry validation. |
| `crates/llm-proxy-core/src/lib.rs` | Export new config and registry types. |
| `crates/llm-proxy-protocol/src/core.rs` | Add `CoreRequest`, `CoreResponse`, `CoreEvent`, `CoreContent`, model/usage/stop/sampling/tool types. |
| `crates/llm-proxy-protocol/src/adapter.rs` | Add client-protocol adapter traits. |
| `crates/llm-proxy-protocol/src/transformer/` | Replace direct pair transforms with adapters to/from core. |
| `crates/llm-proxy-provider/src/client.rs` | Replace `OpenCodeClient` with protocol-neutral HTTP `ProxyClient`. |
| `crates/llm-proxy-provider/src/adapter.rs` | Add provider-adapter traits and adapter registry. |
| `crates/llm-proxy-provider/src/lib.rs` | Export provider adapters and `ProxyClient`. |
| `crates/llm-proxy-server/src/state.rs` | Hold `AppConfig`, provider registry, proxy client, and adapter registries; no fallback handler. |
| `crates/llm-proxy-server/src/routes/messages.rs` | Anthropic inbound route: decode to core, route, provider adapter, encode from core. |
| `crates/llm-proxy-server/src/routes/chat.rs` | OpenAI Chat inbound route follows the same core pipeline. |
| `crates/llm-proxy-server/src/routes/health.rs` | Remove circuit-breaker display. |
| `apps/llm-proxy/src/main.rs` | TOML CLI, provider directory loading, validation, generated examples. |

## Migration Steps

Each step should leave the workspace compiling and tests passing.

| # | What | Scope |
|---|------|-------|
| 1 | Add normalized core chat request/response/event types | `llm-proxy-protocol` |
| 2 | Add client and provider adapter traits | `llm-proxy-protocol`, `llm-proxy-provider` |
| 3 | Add TOML provider/model config and validation alongside old config | `llm-proxy-core` |
| 4 | Implement Anthropic Messages client adapter through core | `llm-proxy-protocol` |
| 5 | Implement OpenAI Chat, OpenAI Responses, Anthropic, and Gemini provider adapters through core | `llm-proxy-provider` + `llm-proxy-protocol` |
| 6 | Add protocol-neutral `ProxyClient` | `llm-proxy-provider` |
| 7 | Rewrite `AppState`, `/v1/messages`, `/v1/chat/completions`, and `main.rs` to use the core pipeline | `llm-proxy-server` + binary |
| 8 | Add streaming `CoreEvent` paths for each adapter | `llm-proxy-protocol`, `llm-proxy-provider`, `llm-proxy-server` |
| 9 | Delete scenario router, fallback, endpoint classifiers, old config, and direct pair transforms | all crates |
| 10 | Full verification gate | workspace |

## Decisions

| Question | Decision |
|----------|----------|
| Provider file location? | `providers/` subdirectory next to `config.toml`. |
| Upstream model override? | Yes, `upstream_model` on `ModelRoute`. |
| Should `ModelRoute` contain endpoint/protocol? | No. Router selects provider/model only. |
| How is provider protocol selected? | Provider registry resolves provider-local `model -> adapter`; adapter protocol must be registered in code. |
| Can new providers be TOML-only? | Only when they use an already implemented provider protocol adapter. |
| Auth style per-endpoint or per-provider? | Per-provider; split provider configs if needed. |
| Server-side model option overrides? | No. Preserve client intent in `CoreRequest`. |
| Unknown model behavior? | `400 Bad Request`. |
| Upstream failure behavior? | `502 Bad Gateway`, no retry in this cut. |
| Streaming normalization? | Required as `CoreEvent`; no text concatenation shortcut. |

## Verification

```sh
# Automated
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace
cargo build --workspace --locked --release

# Manual config checks
llm-proxy init           # creates config.toml + providers/
llm-proxy validate       # validates routes, providers, adapters, env refs
llm-proxy models         # lists client model -> provider/upstream model/adapter
curl -X POST /v1/messages -d '{"model":"unknown","messages":[...]}'  # -> 400
```

Adapter fixtures are part of the gate. Every client and provider adapter needs
golden tests covering:

- plain text request/response
- system prompt
- tool call
- tool result
- streaming text
- streaming tool call
- stop reason mapping
- usage mapping
- reasoning/thinking blocks
- unsupported provider-specific fields

Snapshot the normalized `CoreRequest`, `CoreResponse`, and `CoreEvent` stream
so protocol-normalization regressions are visible.
