# Mini Quirks — Core Principles

CoreChatStream is not passthrough. CoreChatStream is a state machine.

## Reasoning and thinking

Reasoning is stateful across turns.

Observed hacks:

- DeepSeek can require prior assistant `reasoning_content` to be sent
  back on later turns.
- Some proxies inject a single-space reasoning placeholder when the
  provider rejects an empty reasoning field.
- Some providers default to thinking mode unless explicitly disabled.
- Thinking can appear as a dedicated block or inline on a tool call.
- Anthropic thinking signatures and Gemini `thoughtSignature` are
  opaque but need to round-trip.

Design rule:

```text
Reasoning must be explicit CoreChat state.
```

Do not infer it late from raw provider JSON.

Core should preserve:

```text
thinking text
thinking signatures
redacted thinking
provider reasoning ids/signatures
whether reasoning was requested, disabled, or provider-defaulted
```

## Streaming finalization

Providers disagree on when a stream is actually done.

Observed hacks:

- `finish_reason` may arrive before `usage`.
- `usage` may arrive in a separate final chunk.
- some streams end without usage.
- some streams end without an explicit finish event.
- some providers emit metadata-only chunks.
- some proxies synthesize final chunks so clients do not hang.
- some proxies emit `[DONE]` even after an error event.

Design rule:

```text
Usage-only, finish-only, and metadata-only chunks are real CoreChat events.
```

Core stream needs explicit terminal handling:

```text
MessageStart
ContentStart
Delta
Usage
MessageStop
Error
TransportClosed
```

Do not treat “no text delta” as “empty chunk.”

## Tool calls

Tool calls are the messiest streaming case.

Observed hacks:

- tool call name/id may arrive before arguments.
- arguments may arrive as fragmented JSON.
- parallel tool calls are indexed differently by provider.
- some chunks contain empty/ghost tool calls.
- some providers omit IDs and proxies synthesize them.
- final `finish_reason` may say `stop` even though tool calls happened.
- some providers corrupt streams unless a dummy tool exists.
- some providers need tool names sanitized or prefixed.

Design rule:

```text
Tool calls need a dedicated stream accumulator.
```

Core should track:

```text
provider index
core tool_call_id
tool name
arguments delta
arguments buffer
started/stopped state
raw provider fragments
```

Final stop reason should be derived from normalized state, not blindly
trusted from provider text.

## Usage accounting

Token accounting is not portable.

Observed hacks:

- OpenAI prompt tokens may include cached tokens.
- Anthropic input tokens exclude cache reads/writes.
- Gemini reports `promptTokenCount`, `cachedContentTokenCount`,
  `candidatesTokenCount`, and sometimes thinking tokens separately.
- reasoning tokens may be included in output tokens or separate.
- proxies often clamp negative values after cache subtraction.
- stream usage may only be visible in the final chunk.

Design rule:

```text
Usage must be rich and normalized once.
```

Core usage should include:

```rust
struct Usage {
    input_uncached: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}
```

Provider adapters can report raw usage in `provider_meta`, but the
core usage fields should have stable semantics.

## Finish reasons

Finish reasons are not stable.

Observed hacks:

- `tool_calls`, `tool_use`, and `function_call` often mean the same
  thing.
- Gemini `MAX_TOKENS` maps to `max_tokens`.
- unknown finish reasons are often defaulted to `stop`.
- JSON mode may force `stop` even when the provider said something
  else.
- providers may emit `stop` even when tool calls were produced.

Design rule:

```text
Finish-reason mapping must be centralized.
```

Do not let every adapter invent its own mapping.

Core stop reasons should be a small enum:

```text
EndTurn
StopSequence
MaxTokens
ToolUse
ContentFilter
Safety
Error
Unknown
```

Keep the raw provider finish reason in metadata.

## Provider metadata

Some provider data must survive even if core does not understand it.

Observed hacks:

- LiteLLM carries `provider_specific_fields`.
- MCP metadata can travel through stream chunks.
- Responses API may need encoded response/container IDs.
- Gemini needs thought signatures.
- Anthropic needs beta headers and context-management flags.
- provider-specific code interpreter items may replace generic
  function calls.

Design rule:

```text
Every normalized request, response, and event needs provider_meta.
```

This is the pressure-release valve. Use it deliberately.

## Routing affinity

Follow-up requests may need to return to the same provider/model.

Observed hacks:

- LiteLLM encodes provider/model affinity into response IDs.
- container IDs and previous response IDs may carry routing data.
- encrypted content may be wrapped with model IDs.

Design rule:

```text
Plan for reversible IDs.
```

Not needed for the first simple chat path, but required for Responses
API, previous-response flows, containers, and multi-provider routing.

## Retry after stream output

Retries are not equally safe.

Observed hacks:

- before visible output, retry/fallback is usually fine.
- after visible output, retry can duplicate text or corrupt tool state.
- some proxies only retry after output if the last chunk was
  reasoning-only and an explicit flag is enabled.

Design rule:

```text
Retry policy must know whether user-visible output escaped.
```

Core stream state should track:

```text
has_visible_output
has_tool_output
has_reasoning_only_output
```

## Request sanitization

Providers reject fields that other providers accept.

Observed hacks:

- strip unsupported `cache_control`.
- strip or rewrite `dimensions`.
- move `reasoning_effort` into provider-specific `extra_body`.
- rewrite system messages into user/developer messages.
- delete `None` fields before validation.
- inject default schemas for empty tool definitions.
- rewrite `response_format` into tools for providers without native
  JSON mode.

Design rule:

```text
Adapters may sanitize, but core should record what was dropped or rewritten.
```

Use warnings:

```text
TranslationWarning::DroppedField
TranslationWarning::RewrittenField
TranslationWarning::SynthesizedField
```

## Concrete Rust design implications

Core should have:

```text
CoreChatRequest
CoreChatResponse
CoreChatEvent
CoreContent
CoreToolCallState
Usage
StopReason
ReasoningState
ProviderMeta
TranslationWarning
StreamLifecycle
StreamAccumulator
RequestIdentity
CorePolicyTrace
CoreResponseMeta
HeaderMaps
StructuredOutput
ChildRequestScope
EndpointFamily
BatchPolicy
ErrorClassification
FileTransferState
CitationMeta
TokenCountResult
CostEstimate
ProviderRoute
BridgeRequest
BridgeResponse
PassThroughRouteState
LoggingPayload
PromptTemplateState
RoleAlternation
ContinuationPlaceholder
MultimodalBlockMap
```

Provider adapters should have:

```text
request sanitizer
response parser
stream parser
stream accumulator
usage mapper
finish-reason mapper
empty-chunk filter
error mapper
header renderer
structured-output mapper
policy transform tracer
endpoint router
batch result assembler
file/audio multipart builder
error classifier
route canonicalizer
bridge transformer
pass-through logger
prompt template normalizer
quirk flags/capabilities
```

Model config should use capabilities instead of scattered string
checks:

```toml
[models.deepseek]
requires_reasoning_roundtrip = true
defaults_to_reasoning = true
supports_cache_control = false

[models.gemini]
requires_thought_signature = true
supports_parallel_tool_calls = true

[models.qwen]
requires_dummy_tool_for_streaming = true
```

## One-line summary

The clean architecture is:

```text
Protocol -> CoreChat -> Provider
```

But the reliable implementation is:

```text
Protocol -> CoreChat state machine -> Provider quirks at the edge
```
