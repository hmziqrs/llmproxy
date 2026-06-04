# Phase 0 - Current-State Guardrails

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: document and test what exists before replacing it.

### Files

No production files change in this phase.

Add or update tests only if missing:

- `crates/llm-proxy-protocol/src/transformer/request.rs`
- `crates/llm-proxy-protocol/src/transformer/response.rs`
- `crates/llm-proxy-protocol/src/transformer/stream.rs`
- `crates/llm-proxy-server/tests/chat_echo.rs`

### Required checks

Run:

```sh
cargo test --workspace
cargo clippy --all-targets --all-features --locked -- -D warnings
```

If these fail before Phase 1, fix the current code first. Do not start the
normalization migration on a red baseline.

### Baseline facts to preserve during migration

- `/v1/messages` accepts Anthropic Messages request JSON.
- `/v1/messages/count_tokens` returns an Anthropic-style token estimate.
- `/health`, `/ready`, and `/version` are live.
- `routes/chat.rs` exists but `/v1/chat/completions` is currently not mounted.
- The current transformer tests cover text, system prompts, tool calls, tool
  results, thinking blocks, streaming chunks, stop reasons, and usage mapping.
