# Phase 8 - Rewrite `/v1/messages`

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: Anthropic client route uses the core pipeline for both streaming and
non-streaming.

### Files

Rewrite:

```text
crates/llm-proxy-server/src/routes/messages.rs
```

Update:

```text
crates/llm-proxy-server/src/routes/token_count.rs
```

Add or update shared pipeline modules:

```text
crates/llm-proxy-server/src/routes/core_pipeline.rs
crates/llm-proxy-server/src/routes/error_response.rs
```

Do not keep:

- `detect_scenario_from_request`
- `build_scenario_config`
- `handle_non_streaming` with fallback chain
- `execute_non_streaming_request` with `classify_endpoint`
- provider-specific `handle_openai_streaming`
- provider-specific `handle_responses_streaming`
- provider-specific `handle_gemini_streaming`
- raw Anthropic streaming pipe
- `spawn_proxy_task` that converts provider chunks directly to Anthropic SSE

### New non-streaming handler outline

```rust
pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    match handle_messages_inner(state, headers, body).await {
        Ok(response) => response,
        Err(error) => route_error_response(ClientProtocol::Anthropic, error),
    }
}

async fn handle_messages_inner(
    state: AppState,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, RouteError> {
    let ctx = prepare_request(&state, &headers, &body)?;
    let req: MessageRequest = serde_json::from_slice(&body)
        .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;
    req.validate().map_err(RouteError::InvalidRequest)?;

    let core = llm_proxy_protocol::client::anthropic::decode_request(req)
        .map_err(protocol_error_to_route)?;

    if core.stream {
        handle_core_stream(state, ctx, core, ClientProtocol::Anthropic).await
    } else {
        handle_core_once(state, ctx, core, ClientProtocol::Anthropic).await
    }
}
```

Shared `handle_core_once`:

```text
CoreRequest
  -> state.app_config().ok_or_else(|| RouteError::Internal("TOML config required".to_owned()))
  -> state.providers().ok_or_else(|| RouteError::Internal("provider registry required".to_owned()))
  -> resolve_model_route(app_config.models, core.model.requested)
  -> map ModelRouteError::UnknownModel to RouteError::UnknownModel
  -> providers.resolve_adapter_target(...)
  -> ProviderProtocol::parse(...)
  -> state.provider_adapters.get(...)
  -> adapter.encode_request(...)
  -> state.proxy_client.send(...)
  -> adapter.decode_response(...)
  -> anthropic::encode_response(...)
  -> HTTP response
```

Shared `handle_core_stream`:

```text
CoreRequest
  -> state.app_config().ok_or_else(|| RouteError::Internal("TOML config required".to_owned()))
  -> state.providers().ok_or_else(|| RouteError::Internal("provider registry required".to_owned()))
  -> resolve_model_route(app_config.models, core.model.requested)
  -> map ModelRouteError::UnknownModel to RouteError::UnknownModel
  -> providers.resolve_adapter_target(...)
  -> ProviderProtocol::parse(...)
  -> state.provider_adapters.get(...)
  -> adapter.encode_request(...)
  -> state.proxy_client.send_stream(...)
  -> SseFramer.push_chunk(...)
  -> adapter.decode_frame(...)
  -> client adapter encode_event(...)
  -> route-specific SSE/chunk response
  -> adapter.finish()
  -> client adapter terminal event if needed
```

Do not decode provider streams in `/v1/messages` directly. The route chooses the
client protocol encoder only; provider stream parsing belongs to
`SseFramer + ProviderStreamDecoder`.

### Shared route errors

Do not keep an Anthropic-only `ApiError` as the shared pipeline error. Use one
internal error model and encode it per client route.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProtocol {
    Anthropic,
    OpenAiChat,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("upstream error: {status}")]
    Upstream { status: StatusCode, body: String },
    #[error("provider decode error: {0}")]
    ProviderDecode(String),
    #[error("internal error: {0}")]
    Internal(String),
}

pub fn route_error_response(protocol: ClientProtocol, error: RouteError) -> Response<Body>;
```

`/v1/messages` passes `ClientProtocol::Anthropic`. `/v1/chat/completions`
passes `ClientProtocol::OpenAiChat`. The shared pipeline may return
`RouteError`, but only `error_response.rs` knows the client-specific JSON
envelope.

### Token count endpoint

Update `/v1/messages/count_tokens` in this phase so it no longer depends on old
core request helper types that Phase 11 will delete. It remains an Anthropic
client endpoint, but it should parse `MessageRequest`, validate it, decode it
through `client::anthropic::decode_request`, and estimate text tokens from
`CoreRequest.system` and `CoreRequest.messages`.

Do not resolve providers or call upstream for token counting. The endpoint is a
local estimate only.

### Error behavior

- Unknown model: `400 Bad Request`.
- Config/registry resolution error: `500 Internal Server Error` unless it is
  clearly a request model error.
- Upstream `>= 400`: `502 Bad Gateway` with route-specific error envelope.
- Provider adapter decode failure: `502 Bad Gateway`.
- Client adapter decode failure: `400 Bad Request`.

For `/v1/messages`, errors remain Anthropic-shaped.

### Metrics behavior

Keep:

- `metrics.record_request(core.stream)`
- `metrics.record_success(target.upstream_model, latency)`
- `metrics.record_failure()`
- `metrics.record_rate_limited()`
- `metrics.record_deduplicated()`

### Tests

Add server tests with local/mock provider endpoint:

- unknown model returns 400 and does not call upstream
- configured OpenAI Chat provider returns Anthropic response
- configured Responses provider returns Anthropic response
- configured Gemini provider returns Anthropic response
- configured Anthropic provider returns Anthropic response through core, not raw pipe
- upstream 500 returns 502
- invalid JSON returns 400
- request ID header is present on success
- token count endpoint still returns Anthropic-compatible count response
- token count endpoint does not require legacy state

### Gate

```sh
cargo test -p llm-proxy-server messages
cargo test --workspace
```
