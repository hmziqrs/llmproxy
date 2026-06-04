# Phase 2 - Add Client Protocol Adapters

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

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
