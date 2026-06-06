# Protocol Mini

Concise findings from the reference projects in `ref/`.

The proxy should think in protocol families, not provider names. A
provider may support several protocol families, and a client may expect
one specific output shape.

## 1. Protocol families found

| Protocol family | Non-stream output | Stream output | Found in |
|---|---|---|---|
| OpenAI Chat Completions | `chat.completion` with `choices[].message` | `chat.completion.chunk`, `choices[].delta`, final `data: [DONE]` | LiteLLM, llm-api-key-proxy, oc-go-cc |
| Anthropic Messages | `message` with `content[]`, `stop_reason`, `usage` | SSE events: `message_start`, `content_block_start`, `content_block_delta`, `content_block_stop`, `message_delta`, `message_stop` | oc-go-cc, llm-api-key-proxy |
| OpenAI Responses | `response` with `output[]` items | typed events such as `response.output_text.delta`, `response.function_call_arguments.delta`, `response.completed`, `response.failed` | LiteLLM, oc-go-cc |
| Gemini GenerateContent | `candidates[]`, `content.parts[]`, `usageMetadata` | streamed `candidates[]` chunks | oc-go-cc, LiteLLM |
| LiteLLM canonical chat | OpenAI-shaped `ModelResponse` | OpenAI-shaped `ModelResponseStream` | LiteLLM, llm-api-key-proxy |
| Simple text proxy | `{ response, response_model, errors, chat_history }` | none | old llm-proxy |

## 2. Main lesson

Do not make one giant universal core type.

Use endpoint-family cores:

```text
CoreChat
CoreResponses
CoreEmbeddings
CoreImages
CoreAudio
CoreRerank
```

For the coding-agent proxy, start with:

```text
CoreChat
CoreChatStream
```

That covers the important paths:

```text
OpenAI Chat       -> CoreChat -> Anthropic Messages
Anthropic Messages -> CoreChat -> OpenAI Chat
OpenAI Responses  -> CoreChat/CoreResponses -> Anthropic Messages
Gemini            -> CoreChat -> OpenAI/Anthropic
```

## 3. Recommended v1 protocols

Priority order:

1. Anthropic Messages
2. OpenAI Chat Completions
3. OpenAI Responses
4. Gemini GenerateContent

Reason:

- Claude Code-style clients want Anthropic Messages.
- OpenAI-compatible clients/providers want Chat Completions.
- Codex/OpenAI agent surfaces increasingly use Responses.
- Gemini and Google-backed tools use GenerateContent.

Ignore old text completions for v1 unless a specific client requires
them.

## 4. Core chat response

Core should be block-based, not OpenAI-shaped or Anthropic-shaped.

```rust
struct CoreChatResponse {
    id: Option<String>,
    model: ModelRef,
    content: Vec<CoreContent>,
    stop_reason: StopReason,
    usage: Usage,
    provider_meta: serde_json::Map<String, serde_json::Value>,
}
```

Core content should include:

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

## 5. Core chat stream

Streaming should be normalized as events.

```rust
enum CoreChatEvent {
    MessageStart {
        id: Option<String>,
        model: ModelRef,
    },
    ContentStart {
        index: usize,
        kind: ContentKind,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        args_delta: String,
    },
    ToolCallStop {
        index: usize,
    },
    UsageDelta {
        usage: Usage,
    },
    MessageStop {
        stop_reason: StopReason,
        stop_sequence: Option<String>,
    },
    Error {
        error: CoreError,
    },
    Ping,
}
```

This maps cleanly to:

- Anthropic SSE events
- OpenAI chat completion chunks
- OpenAI Responses typed events
- Gemini streamed candidates

## 6. What each ref teaches

### oc-go-cc

Inbound is Anthropic Messages. Upstream can be OpenAI Chat
Completions, OpenAI Responses, Gemini, or raw Anthropic. Output is
always Anthropic-shaped for clients.

Useful lesson:

```text
Provider streams need state.
```

Tool calls, thinking blocks, usage, and stop reasons arrive in pieces.
The adapter has to buffer and emit a coherent client stream.

### llm-api-key-proxy

Inbound supports OpenAI Chat Completions and Anthropic Messages.
Internally it mostly uses LiteLLM/OpenAI-shaped chat responses.
Anthropic compatibility is implemented as an edge translation.

Useful lesson:

```text
Keep routing separate from translation.
```

Credential rotation and provider selection should not leak into the
protocol adapters.

### LiteLLM

LiteLLM uses several canonical envelopes, not one universal schema:

- chat completions
- text completions
- embeddings
- Responses API
- images
- audio
- rerank/search/etc.

Useful lesson:

```text
Normalize by endpoint family.
```

A chat request and an embedding request should not share one massive
core DTO.

### old llm-proxy

This is a simple library-level router. It collapses all providers into:

```text
response text + response model + errors + chat history
```

Useful for routing/failover ideas, but not useful as a wire-protocol
model for coding agents.

## 7. Design rule

Do not build direct protocol pairs:

```text
OpenAI -> Anthropic
Anthropic -> OpenAI
Gemini -> OpenAI
Responses -> Anthropic
```

Build:

```text
Protocol -> CoreChat -> Protocol
```

Each adapter only knows:

```text
its protocol <-> CoreChat
```

No adapter should know another adapter exists.
