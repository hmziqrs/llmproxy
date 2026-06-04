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
