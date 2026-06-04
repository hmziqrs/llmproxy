# Phase 4 - Add Protocol-Neutral Transport

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: replace OpenCode-specific HTTP transport without changing live routes yet.

### Files

Add:

```text
crates/llm-proxy-provider/src/error.rs
crates/llm-proxy-provider/src/transport.rs
crates/llm-proxy-provider/src/sse.rs
```

Update:

```text
crates/llm-proxy-provider/src/client.rs
crates/llm-proxy-provider/src/lib.rs
```

Do not delete `client.rs` in this phase.

### Provider error

Move `ProviderError` out of the old OpenCode client before new transport and
adapters depend on it:

```text
crates/llm-proxy-provider/src/client.rs -> crates/llm-proxy-provider/src/error.rs
```

Re-export it from `lib.rs`:

```rust
pub mod error;
pub use error::ProviderError;
```

Then update old `client.rs`, new `transport.rs`, and provider adapters to import
`crate::ProviderError`. This prevents Phase 11 from accidentally deleting the
shared provider error when `OpenCodeClient` is removed.

### Types

```rust
#[derive(Debug, Clone)]
pub struct ProxyClient {
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct ProxyRequest {
    pub url: String,
    pub auth: AuthHeaders,
    pub body: Vec<u8>,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct AuthHeaders {
    pub style: llm_proxy_core::AuthStyle,
    pub api_key: String,
}
```

Methods:

```rust
impl ProxyClient {
    pub fn new() -> Self;

    pub async fn send(&self, req: ProxyRequest) -> Result<Vec<u8>, ProviderError>;

    pub async fn send_stream(
        &self,
        req: ProxyRequest,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static>
        >,
        ProviderError,
    >;
}
```

Transport rules:

- Always set `Content-Type: application/json`.
- Set `Accept: text/event-stream` only for streams.
- `AuthStyle::Bearer` sets `Authorization: Bearer <key>`.
- `AuthStyle::XApiKey` sets `x-api-key: <key>`.
- `AuthStyle::Both` sets both headers.
- HTTP status `>= 400` returns `ProviderError::Api { status, body }`.
- The transport does not know protocol names.
- The transport does not serialize typed request structs. It sends bytes.

### SSE framing helper

Add a small buffered SSE parser in `sse.rs`. Do not split raw HTTP byte chunks
with `str::lines()` in route handlers or adapters. Provider streams can split a
single SSE frame across multiple TCP chunks, and some providers emit comments,
event names, ids, or multi-line `data:` fields.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseFramer {
    // buffered bytes and partially parsed frame state
}

impl SseFramer {
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ProviderError>;
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, ProviderError>;
}
```

Framing rules:

- preserve partial frames across byte chunks
- support `\n` and `\r\n`
- ignore comment lines beginning with `:`
- collect repeated `data:` lines with newline separators
- preserve optional `event:` and `id:` fields
- emit a frame only after a blank line or final `finish()`
- treat `data: [DONE]` as a normal terminal frame for adapters to interpret
- return a provider error for invalid UTF-8

### Tests

Use a local axum test server or `wiremock` if added. If avoiding a new
dependency, use axum in a dev-only integration test.

Test:

- bearer header set
- x-api-key header set
- both headers set
- error status returns `ProviderError::Api`
- stream request sets `Accept: text/event-stream`
- SSE framer handles partial frames split across chunks
- SSE framer handles multi-line data fields
- SSE framer preserves `[DONE]`

### Gate

```sh
cargo test -p llm-proxy-provider transport
cargo test --workspace
```
