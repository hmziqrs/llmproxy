# Phase 5 - Add Provider Protocol Adapters

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

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
    pub fn protocol_names(&self) -> Vec<&'static str>;
    pub fn has_protocol_name(&self, protocol: &str) -> bool;
    pub fn get(&self, protocol: ProviderProtocol) -> Option<&ProviderAdapter>;
}
```

Add parse/name tests for every TOML protocol string used in Phase 3 examples.

### Scope guardrails

Provider adapters must not import client adapters or implement direct
client-protocol behavior. They only translate:

```text
CoreRequest -> provider request JSON/URL
provider response JSON/SSE -> CoreResponse/CoreEvent
```

They must not inspect client protocol names, route errors, or server state.

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
- DeepSeek/Kimi provider quirks require an explicit compatibility profile in
  provider config or a separate adapter. Do not infer quirks from provider/model
  string matching inside the generic OpenAI Chat adapter.

### Anthropic provider adapter

For upstream Anthropic-compatible providers:

```text
CoreRequest -> anthropic::MessageRequest
anthropic::MessageResponse -> CoreResponse
anthropic::MessageEvent stream -> CoreEvent stream
```

Anthropic requires tool names to match `^[a-zA-Z0-9_-]{1,128}$` and rejects non-object `input_schema.type`. When encoding `CoreTool`/`CoreContent::ToolUse` to Anthropic, the adapter must: sanitize tool names with collision-safe, REVERSIBLE rewrites (keep a reverse map only for names that were actually rewritten, so `tool_use.name` on the response path maps back correctly); sanitize `tool_use_id` to the provider regex; and coerce a non-object `input_schema.type` to `object` (inserting an empty `properties` map when needed). This sanitization lives in the Anthropic provider adapter only — it must not leak into core or client adapters.

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

### Provider hints passthrough

Provider adapters MAY merge allowlisted entries from `CoreRequest.provider_hints.raw` into the outgoing provider request body (for example OpenAI `stream_options`, or OpenRouter-style `route`/`models`/`transforms`). Each adapter documents which hint keys it forwards. An adapter must never forward a client-protocol-specific hint into a mismatched provider, and must never let an unknown hint silently change behavior. Hints with no provider mapping are ignored (and may be `tracing::warn!`-logged).

### Lossy translation

Every unsupported feature must choose one of these outcomes:

1. Reject with `ProviderError` if sending it would change behavior silently.
2. Omit with an explicit warning in `provider_meta` when the omission is safe.
3. Preserve opaque provider-specific data in `provider_meta`.

Do not special-case Provider A inside Protocol B's adapter unless it is selected
through an explicit compatibility profile or separate adapter.

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
- model alias response preserves `CoreResponse.model.requested =
  target.requested_model`
- provider adapters encode `target.upstream_model` for provider requests
- `CoreRequest.stream` maps to `ProxyRequest.stream`
- stop reason mapping
- stop sequence mapping where supported
- usage mapping
- Thinking, RedactedThinking, Refusal, and reasoning fields
- multimodal content supported/unsupported behavior
- streaming text
- streaming tool call
- full stream `CoreEvent` sequence with `MessageStart`, `ContentStart`,
  `UsageDelta`, and `MessageStop`
- content indexes and partial tool-call buffering
- unknown provider events
- streaming terminal frame handling
- malformed stream frame behavior
- provider-specific unsupported field behavior
- each provider stream decoder documents which `CoreEvent` variants it emits, and tests `Ping`, `ThinkingDelta`, and `Error`/`response.failed` handling (emit, or intentionally never), mirroring the Phase 2 client "every CoreEvent variant maps or errors intentionally" test
- empty-chunk classification: usage-only, finish-reason-only, and heartbeat/ping chunks are NOT dropped just because they carry no text — they map to `UsageDelta`, `MessageStop`, and `Ping` respectively (empty is a protocol decision, not a generic truthy check)
- OpenAI Chat adapter decode tolerates both `reasoning_content` and `reasoning` (including streamed `delta.reasoning`), mapping both to `CoreContent::Thinking` / `ThinkingDelta`, since OpenAI-compatible providers alias the field

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
- source guard: `adapter/*.rs` imports none of `llm_proxy_protocol::client`, `llm_proxy_server`, route errors, or server state (proves the `protocol-normalization.md` §9 invariant that adding a provider changes no client code; mirrors the Phase 4 transport source guard).

Each non-stream fixture case should use:

```text
input.json
core.json
output.json
```

Each stream fixture case should use:

```text
input.sse
core-events.json
output.sse
```

### Gate

```sh
cargo test -p llm-proxy-provider
cargo test --workspace
```
