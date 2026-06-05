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

`ProviderError` must include variants for:

- JSON encode/decode errors
- HTTP transport errors
- API status errors with status/body
- SSE/framing errors
- invalid UTF-8 in streamed bytes

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
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send + 'static>
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
- Streaming HTTP status `>= 400` also returns `ProviderError::Api` before any
  byte stream is exposed.
- Stream item errors are wrapped as `ProviderError`; adapters and routes should
  not see raw `reqwest::Error`.
- The transport does not know protocol names.
- The transport does not serialize typed request structs. It sends bytes.
- Non-stream requests must not set `Accept: text/event-stream`.
- `ProxyRequest.body` is sent byte-for-byte.
- `AuthHeaders` and `ProxyRequest` must NOT derive a `Debug` that prints `api_key`. Provide a manual `Debug` impl that redacts the key (render it as `"***"`), so `tracing::debug!(?req)` or snapshot output never leaks the secret.

### Neutrality guardrails

`transport.rs` must not import or mention:

- `EndpointType`
- `classify_endpoint`
- `OpenCodeClient`
- model IDs or provider names
- `llm_proxy_protocol`

The transport only sends prepared bytes to a prepared URL with prepared auth
headers. Provider adapters own protocol-specific request/response behavior.

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

### Cancellation and disconnect

When the client disconnects mid-stream, dropping the stream returned by `send_stream` must abort the in-flight upstream request and close its response body. Otherwise the proxy leaks sockets and keeps paying for abandoned upstream calls (`docs/research/quirks/05-routing-lifecycle.md`).

- The stream future owns the `reqwest` response; dropping it drops the connection.
- `send_stream`'s returned stream is cancel-safe: dropping it cancels upstream. Do not spawn a detached task that outlives the consumer.
- Client disconnect is a terminal lifecycle event, distinct from an upstream error.

### Tests

Use a local axum test server or `wiremock` if added. If avoiding a new
dependency, use axum in a dev-only integration test.

Test:

- bearer header set
- x-api-key header set
- both headers set
- error status returns `ProviderError::Api`
- streaming error status returns `ProviderError::Api`
- stream request sets `Accept: text/event-stream`
- non-stream request does not set `Accept: text/event-stream`
- request body is sent byte-for-byte
- SSE framer handles partial frames split across chunks
- SSE framer handles multi-line data fields
- SSE framer handles `\r\n`
- SSE framer ignores comments
- SSE framer preserves `event:` and `id:`
- SSE framer emits a trailing frame from `finish()`
- SSE framer respects blank-line frame boundaries
- SSE framer preserves `[DONE]`
- SSE framer rejects invalid UTF-8 with `ProviderError`
- source guard checks confirm `transport.rs` has no endpoint classification,
  provider model, or protocol imports
- `auth_headers_debug_redacts_api_key` and `proxy_request_debug_redacts_api_key`
- dropping the returned stream aborts the upstream request (no detached task keeps it alive)

### Gate

```sh
cargo test -p llm-proxy-provider transport
cargo test --workspace
```
