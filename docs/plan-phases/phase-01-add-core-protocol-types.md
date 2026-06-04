# Phase 1 - Add Core Protocol Types

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

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
