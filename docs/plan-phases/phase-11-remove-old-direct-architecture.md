# Phase 11 - Remove Old Direct Architecture

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: delete obsolete code only after both live routes use the core pipeline.

### Preconditions

Start Phase 11 only after:

- `/v1/messages` uses `CoreRequest/CoreResponse/CoreEvent` for streaming and
  non-streaming.
- `/v1/chat/completions` uses the same shared core pipeline.
- `/v1/messages/count_tokens` no longer depends on legacy state.
- CLI `serve` no longer starts with old JSON config.
- Phase 8, Phase 9, and Phase 10 gates pass.

### Delete

```text
crates/llm-proxy-core/src/router/
```

Delete only legacy scenario/fallback router code. Preserve the new model-route
lookup and `ProviderTarget` behavior required by the core pipeline.

Remove exports from:

```text
crates/llm-proxy-core/src/lib.rs
```

Delete or empty:

```text
crates/llm-proxy-protocol/src/transformer/request.rs
crates/llm-proxy-protocol/src/transformer/response.rs
crates/llm-proxy-protocol/src/transformer/stream.rs
```

Prefer deleting `transformer/` entirely once adapters cover all tests.

Remove protocol exports and dependencies tied to the old direct architecture:

```text
crates/llm-proxy-protocol/src/lib.rs
crates/llm-proxy-protocol/Cargo.toml
```

Required cleanup:

- remove `pub mod transformer`
- remove `llm-proxy-core` from `llm-proxy-protocol` dependencies
- keep protocol crate independent of core crate; normalized core types now live
  inside `llm-proxy-protocol::core`
- verify no protocol module imports `llm_proxy_core::*`

Delete from provider:

```text
OpenCodeClient
EndpointType
classify_endpoint
is_anthropic_model
is_gemini_model
is_responses_model
is_zen
provider
```

Delete old config structs:

```text
Config
ModelConfig
OpenCodeGoConfig
OpenCodeZenConfig
LoggingConfig
```

Only delete `LoggingConfig` if replacement `ServerConfig.log_level` is live and
all code uses it.

Remove from server:

```text
ModelRouter
fallback_handler
circuit_breakers in health response
scenario logs
fallback-chain logic
LegacyState
```

After old config structs are removed, verify `apps/llm-proxy` no longer serves
fallback/scenario JSON and no longer treats `OC_GO_CC_CONFIG` as a live config
path.

Make the new runtime fields non-optional:

```rust
pub app_config: Arc<AppConfig>,
pub providers: Arc<ProviderRegistry>,
```

Remove `app_config()` and `providers()` option helpers if they only existed to
bridge Phase 7 JSON compatibility.

### Tests to delete or rewrite

Before deleting legacy transformer tests, verify replacement coverage exists
for:

- plain text
- system prompts
- tool use and tool result
- reasoning/thinking
- stop reason and stop sequence
- usage
- streaming text and streaming tool calls
- unsupported fields

Before deleting `transformer/stream.rs`, verify replacement stream tests cover:

- provider decoder state
- tool-call deltas
- usage and stop mapping
- unknown provider events
- disconnect errors
- client SSE encoding

Delete tests that assert scenario routing, fallback, and endpoint
classification.

Rewrite tests that assert useful behavior through the new architecture:

- scenario test for `glm-5.1` becomes route table lookup test
- endpoint classification test becomes config-driven
  provider-local-model -> adapter -> protocol resolution test, including
  unknown model and adapter failures
- stream proxy test becomes provider event decoder + client event encoder tests
- routing/config tests prove only provider and upstream model are selected
- routing/config tests prove sampling, tools, stream, metadata, reasoning, and
  cache intent are not overridden

### Gate

```sh
cargo test --workspace
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
! rg -n "transformer|detect_scenario|route_for_streaming|FallbackHandler|OpenCodeClient|EndpointType|classify_endpoint" crates apps
! rg -n "handle_openai_streaming|handle_responses_streaming|handle_gemini_streaming|spawn_proxy_task" crates/llm-proxy-server/src/routes
```

The `rg` checks should return no live-route or live-config references. If a
string remains in a deleted-code migration test or documentation-only context,
document why it is not part of runtime behavior.
