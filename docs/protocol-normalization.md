# Protocol Normalization

The proxy should not implement direct protocol-to-protocol converters.

Do not build this:

```text
OpenAI -> Anthropic
Anthropic -> OpenAI
OpenAI -> Gemini
Gemini -> OpenAI
Anthropic -> Gemini
Gemini -> Anthropic
```

Build this:

```text
OpenAI    -> Core -> Anthropic
Anthropic -> Core -> OpenAI
Gemini    -> Core -> OpenAI
Codex     -> Core -> OpenCode
Claude    -> Core -> ChatGPT/Codex
```

Every client protocol converts into one normalized internal protocol.
Every provider converts from that internal protocol into its own wire
format.

This turns the problem from `N * N` converters into `2 * N` adapters.

## 1. The rule

There are only two valid translation paths:

```text
Inbound client protocol -> Core protocol
Core protocol -> Provider protocol
```

No adapter should know about another adapter.

An OpenAI inbound adapter should not know Anthropic exists. An
Anthropic provider adapter should not know Codex exists.

The core protocol is the boundary.

## 2. Terms

### Client protocol

The format spoken by the tool calling the local proxy.

Examples:

- OpenAI Chat Completions
- OpenAI Responses
- Anthropic Messages
- Codex-style client requests
- Claude Code-style client requests
- OpenCode-style client requests
- Kilo Code-style client requests

### Provider protocol

The format spoken by the upstream backend.

Examples:

- OpenAI API
- Anthropic API
- Gemini API
- OpenRouter
- Ollama
- LM Studio
- OpenCode providers
- ChatGPT/Codex subscription bridge

### Core protocol

The proxy's normalized internal representation.

This is not OpenAI-shaped. This is not Anthropic-shaped. It is the
proxy's own contract between layers.

## 3. Request normalization

A normalized request should model intent, not provider JSON.

Core request shape:

```rust
struct CoreRequest {
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
```

The request should preserve the things that affect behavior:

- messages
- system instructions
- text content
- image/document/audio/video content
- tool definitions
- tool choice
- temperature/top-p/max tokens
- reasoning/thinking configuration
- cache markers
- requested streaming mode
- caller metadata

It should not preserve irrelevant wire-format nesting.

For example, OpenAI's:

```text
choices[0].message.tool_calls[0].function.arguments
```

should become:

```text
ToolUse { id, name, input }
```

## 4. Response normalization

Core responses should be block-based.

```rust
struct CoreResponse {
    id: Option<String>,
    model: ModelRef,
    content: Vec<CoreContent>,
    stop_reason: StopReason,
    usage: Usage,
    provider_meta: Map<String, Value>,
}
```

Core content should support at least:

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

Provider-specific fields that are useful but not canonical go into
`provider_meta`.

## 5. Streaming normalization

Streaming must be normalized as events.

Do not normalize streaming by concatenating text and pretending the
response was non-streaming. Tool calls, reasoning blocks, stop reasons,
usage, and errors all arrive incrementally.

Core stream events:

```rust
enum CoreEvent {
    MessageStart { id: Option<String>, model: ModelRef },
    ContentStart { index: usize, kind: ContentKind },
    TextDelta { index: usize, text: String },
    ToolCallStart { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, args_delta: String },
    ToolCallStop { index: usize },
    ThinkingDelta { index: usize, text: String },
    UsageDelta { usage: Usage },
    MessageStop { stop_reason: StopReason, stop_sequence: Option<String> },
    Error { error: CoreError },
    Ping,
}
```

Provider stream parsers may need local state. That state belongs inside
the adapter, not in the router.

Examples:

- buffering partial tool-call JSON
- mapping provider-specific content indexes
- carrying Anthropic thinking signatures
- tolerating unknown provider event types
- turning upstream disconnects into clean client errors

## 6. Adapter responsibilities

Inbound protocol adapter:

```text
client request JSON -> CoreRequest
CoreResponse/CoreEvent -> client response JSON/SSE
```

Provider adapter:

```text
CoreRequest -> provider request JSON
provider response JSON/SSE -> CoreResponse/CoreEvent
```

Router:

```text
CoreRequest -> selected provider/model
```

The router should not translate protocol fields. It should only choose
where the request goes.

## 7. Lossy translation

Some provider features will not map perfectly.

Allowed outcomes:

1. Add a canonical core field if the feature is broadly useful.
2. Store opaque provider-specific data in `provider_meta`.
3. Drop the field and record a warning.

Disallowed outcome:

```text
Special-case Provider A inside Protocol B's adapter.
```

That breaks the whole architecture.

## 8. Adding a new protocol

Adding a new client protocol should require:

```text
1. Add protocol/<name>.rs
2. Decode its requests into CoreRequest
3. Encode CoreResponse/CoreEvent back to that protocol
4. Add routes
5. Add fixtures
```

No provider code should change.

## 9. Adding a new provider

Adding a new provider should require:

```text
1. Add provider/<name>/wire.rs
2. Add provider/<name>/client.rs
3. Convert CoreRequest into provider JSON
4. Convert provider JSON/SSE into CoreResponse/CoreEvent
5. Register config
6. Add fixtures
```

No client protocol code should change.

## 10. Golden test rule

Every adapter needs fixtures.

Minimum fixture types:

- plain text request/response
- system prompt
- tool call
- tool result
- streaming text
- streaming tool call
- stop reason mapping
- usage mapping
- provider-specific unsupported field

Protocol normalization bugs are usually silent. Snapshot the core
request, core response, and core stream events.

## 11. One-line summary

The proxy is not `A-to-B`.

The proxy is:

```text
A -> Core -> B
```

That is the whole design.
