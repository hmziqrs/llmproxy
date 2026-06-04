# Phase 9 - Mount Real `/v1/chat/completions`

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

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

`routes/chat.rs` must not import or use legacy `transformer::*`,
`OpenCodeClient`, provider wire DTOs, endpoint classification, scenario/fallback
code, or direct provider resolution. It only decodes OpenAI Chat to
`CoreRequest`, calls the shared core pipeline, and encodes OpenAI Chat output.

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

Status mapping:

- invalid JSON or client adapter decode failure: `400 Bad Request`
- unknown model: `400 Bad Request`
- provider registry/config or compiled adapter failure: `500 Internal Server Error`
- upstream `>= 400` or provider decode failure: `502 Bad Gateway`

All of these must use the OpenAI error envelope.

### Tests

- route is mounted, no longer 404
- non-streaming text request returns OpenAI-shaped response
- non-streaming response asserts `object="chat.completion"`, `id`, `model`,
  `choices[].finish_reason`, text content, and `usage`
- tool-call core response returns `tool_calls`
- route preserves temperature, top-p, max tokens, tools, tool choice, metadata,
  stream, stream options/provider hints, reasoning/thinking, and cache markers
- unknown model returns OpenAI-shaped 400
- invalid JSON/client decode failure returns OpenAI-shaped 400
- upstream failure returns OpenAI-shaped 502
- provider decode failure returns OpenAI-shaped 502
- streaming response emits `chat.completion.chunk`
- streaming response sets SSE content type
- `stream: true` uses shared streaming pipeline
- streaming tool-call start/delta/stop maps to `choices[].delta.tool_calls`
- streaming usage maps when requested
- streaming stop reason maps to `finish_reason`
- stream errors become OpenAI-shaped stream errors or route errors
- streaming response ends with `[DONE]`
- no Anthropic error envelope appears on this route
- source guard confirms `routes/chat.rs` has no legacy transformer,
  endpoint-classifier, fallback, or direct-provider execution path

### Gate

```sh
cargo test -p llm-proxy-server chat
cargo test --workspace
```

Phase 9 completes the OpenAI Chat route, but the runtime architecture is not
complete until Phase 11 removes the old direct architecture, and fixture
coverage is not complete until Phase 12.
