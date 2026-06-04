# Phase 11 - Remove Old Direct Architecture

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: delete obsolete code only after both live routes use the core pipeline.

### Delete

```text
crates/llm-proxy-core/src/router/
```

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

### Tests to delete or rewrite

Delete tests that assert scenario routing, fallback, and endpoint
classification.

Rewrite tests that assert useful behavior through the new architecture:

- scenario test for `glm-5.1` becomes route table lookup test
- endpoint classification test becomes provider model adapter lookup test
- stream proxy test becomes provider event decoder + client event encoder tests

### Gate

```sh
cargo test --workspace
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```
