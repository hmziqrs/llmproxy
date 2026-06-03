# Architecture

The engine as a whole: shape, IR, provider layer, streaming, and how we
get to "any provider" without writing 100 bespoke adapters.

## 1. Engine shape

Three plausible shapes:

| Shape | When it fits | Cost |
|---|---|---|
| **Single binary, in-process** | Personal/team proxy, sidecar, embedded | Simplest ops; one process holds all state |
| **Split: router daemon + provider sidecars** | Multi-tenant, mixed clouds, per-provider failover isolation | More moving pieces; language-agnostic providers |
| **Distributed gateway (data plane + control plane)** | SaaS, fleet of gateways, central config | Heavy; over-engineered for v1 |

The client (Claude Code, Codex, anything else) is a CLI that talks
HTTP to the engine. v1 is shape 1.

## 2. The canonical IR

The hard single decision. If you write `Anthropic↔OpenAI`,
`Anthropic↔Gemini`, `OpenAI↔Gemini` directly you get N squared
converters and every new provider is N rewrites. The pattern that
scales is one canonical intermediate representation plus two
converters per provider (in + out). This is how LiteLLM, OpenRouter,
Portkey, and Vercel AI Gateway all work.

Three schools of thought on the IR:

- **A. Superset / union.** Every field from every provider is
  representable. Round-trip faithful, but the type is large and
  growing. Most of LiteLLM's complexity lives here.
- **B. Minimum viable.** Only fields you can actually act on. Smaller,
  but silently drops information (Anthropic `cache_control`, Gemini
  `thoughtSignature`, OpenAI `prompt_cache_key`).
- **C. Asymmetric.** Separate `RequestIR` and `ResponseIR`, each shaped
  for its job. Streaming gets a third type (`EventIR`).

**C is the right answer for a proxy.** The IR models the protocol
between layers, not the data.

### Shape (shorthand)

```
RequestIR   = messages + tools + sampling + system
            + (optional) thinking_config
            + (optional) provider_hints (cache markers, route tags)

ResponseIR  = id + model + content_blocks
            + stop_reason + usage
            + raw_provider_meta

EventIR     = MessageStart
            | ContentStart { type, index }
            | ContentDelta { kind, payload }
            | ContentStop
            | ToolUseStart
            | ToolUseDelta
            | ToolUseStop
            | MessageDelta { stop_reason, usage }
            | MessageStop
            | Error
            | Ping
```

Lives in `crates/llm-proxy-protocol`.

### Content blocks (canonical)

The polymorphic field that has to handle every provider's shapes:

```
Text           { text, cache_control? }
Image          { source: url | base64 | file_id }
Document       { source }       // Anthropic PDF
Audio          { source }       // Gemini
ToolUse        { id, name, input }
ToolResult     { tool_use_id, content, is_error }
Thinking       { text, signature? }     // Anthropic + o-series
RedactedThinking { data }                // Anthropic
Refusal        { message }              // OpenAI
Video          { source }               // Gemini
```

### Stop reasons

Centralize the mapping. The full set the engine has to know about:

```
end_turn, stop, STOP, model_length, tool_use, function_call,
content_filter, safety, max_tokens, other
```

### Usage tokens

Asymmetric across providers and the source of silent context-budget
bugs:

| Provider | Input | Cached | Reasoning |
|---|---|---|---|
| Anthropic | `input_tokens` (non-cached only) | `cache_creation_input_tokens`, `cache_read_input_tokens` | (folded into output) |
| OpenAI Chat | `prompt_tokens` (total) | `prompt_tokens_details.cached_tokens` | (separate field, sometimes) |
| OpenAI Responses | same as Chat | same | `output_tokens_details.reasoning_tokens` |
| Gemini | `promptTokenCount` | `cachedContentTokenCount` | `thoughtsTokenCount` |

Canonical usage:

```
input, output, cache_read, cache_creation, reasoning
```

The OpenAI `prompt_tokens - cached` math from the Go reference
(`internal/transformer/response.go:60-67`) is the same trap in
production: if `input_tokens` is the total instead of the non-cached
count, clients' local context counters over-budget and trigger
auto-compact early. Preserve the canonical semantics.

## 3. Provider layer

The trait surface should be small and stable. Internally, every
provider is two files:

- `wire.rs` - pure IR <-> JSON, no I/O. Unit-testable with fixtures.
- `client.rs` - HTTP I/O, auth, retries, calls into `wire.rs`.

Lives in `crates/llm-proxy-provider`.

```
trait Provider:
    fn name() -> &'static str
    fn auth() -> &Auth
    fn classify_endpoint(model) -> Endpoint
    async fn send(ctx, ir: &RequestIR) -> ResponseIR
    async fn stream(ctx, ir: &RequestIR) -> Stream<EventIR>
```

A registry maps `provider_id` to a factory. Two registration
mechanisms:

- **Built-in features** (`provider-anthropic`, `provider-openai`,
  `provider-gemini`, `provider-bedrock`, `provider-vertex`) for the
  providers with hard problems. Compile in only what you need.
- **Generic OpenAI-compat provider** that takes `base_url`, `auth_style`,
  `model_list`, and quirks from config. Covers Together, Groq,
  Fireworks, DeepInfra, OpenRouter, Ollama, vLLM, LM Studio, and
  basically every "we resell OpenAI's API" shop.

### Endpoint classification

A single provider can offer several wire formats. The trait exposes
`Endpoint` so a request can be routed correctly:

```
ChatCompletions   // /v1/chat/completions
Responses         // /v1/responses (Codex, etc.)
AnthropicMessages // /v1/messages
Gemini            // /v1/models/{id}:generateContent
```

## 4. Streaming

Where naive implementations die. Four concerns to plan up front.

### 1. Provider SSE is not a stable contract

OpenAI Responses occasionally emits unknown event types. Anthropic
interleaves `ping` events. Gemini does its own framing. The parser
must tolerate unknown event types and never panic on malformed input.

### 2. State across chunks

Tool calls stream incrementally. First chunk has `id` + `name`,
subsequent chunks have argument fragments. Keep a per-call buffer.
Same for Anthropic `input_json_delta` - accumulate a JSON string and
may have to validate it before emitting `content_block_stop`.

### 3. Cancellation in both directions

Client disconnect aborts the upstream HTTP. Upstream error mid-stream
emits a clean Anthropic-format `error` event and closes; do not
half-truncate. Use `tokio::select!` over `client_canceled`,
`upstream_next`, `heartbeat_tick`.

### 4. Reasoning/thinking round-trip

Anthropic's `signature` is opaque but required on subsequent turns.
OpenAI's `reasoning_tokens` are summarized on the way back. Naive
translation breaks multi-turn reasoning. The IR treats reasoning as a
first-class content kind with a provider-typed opaque payload. Do not
try to normalize it away.

The Go reference (`ref/oc-go-cc/internal/transformer/stream.go`)
deliberately avoids `bufio.Reader` on the response body to minimize
TTFT. The same discipline applies in Rust: use raw `read()` plus a
manual newline scan, not `BufReader::read_line`.

## 5. Extensibility: getting to "any provider"

The realistic total market breaks into five categories with very
different costs:

| Category | Count | Cost |
|---|---|---|
| OpenAI Chat Completions compatible | 30+ | Trivial - one class + config |
| OpenAI Responses compatible | ~3 | Medium - share code with Chat where possible |
| Anthropic Messages native | 3-5 (Anthropic, Bedrock, Vertex, OpenRouter passthrough) | Medium - same code, different auth/URLs |
| Gemini generateContent | 1-2 (Google direct, Vertex) | Medium - unique wire format, thinking/grounding quirks |
| Truly weird (Cohere, AI21, etc.) | ~10 | High - bespoke |

Net: a credible "100 providers" claim costs 5-7 real implementations
plus the generic OpenAI-compat class. Anything beyond that is a
config file.

### Per-provider config presets

Generic OpenAI-compat class plus bundled YAML/TOML presets:

```yaml
# presets/together.yaml
provider: openai-compat
base_url: https://api.together.xyz/v1
auth: { type: bearer, from_env: TOGETHER_API_KEY }
models:
  - id: meta-llama/Llama-3.3-70B-Instruct-Turbo
    context: 131072
    pricing: { input: 0.88, output: 0.88 }   # per 1M tokens USD
  - id: Qwen/Qwen2.5-Coder-32B-Instruct
    context: 32768
```

The cost of adding a new provider drops to: write a preset file, ship.
