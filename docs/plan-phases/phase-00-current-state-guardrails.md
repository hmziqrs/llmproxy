# Phase 0 - Current-State Guardrails

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: document and test what exists before replacing it.

This is a characterization phase only. Existing direct transformers, scenario
routing, endpoint classification, and config-overrides-client behavior may be
tested here so the migration has a known baseline, but they are legacy behavior
and must not be copied into new code after this phase.

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
- `/v1/messages` currently still performs scenario detection, endpoint
  classification, fallback handling, and direct Anthropic-to-provider
  transformation. Preserve this only as a legacy characterization fact.
- `/v1/messages/count_tokens` returns an Anthropic-style token estimate.
- `/health`, `/ready`, and `/version` are live.
- `routes/chat.rs` exists but `/v1/chat/completions` is currently not mounted.
- The current transformer tests cover text, system prompts, tool calls, tool
  results, thinking blocks, streaming chunks, stop reasons, and usage mapping.
- Current transformer tests are legacy characterization tests. New tests after
  Phase 0 must target `wire -> core -> wire`, not direct protocol pairs.
- Current config-driven sampling overrides are legacy behavior. New core and
  adapter tests must preserve client intent in `CoreRequest`.

### Test inventory

Before Phase 1, inventory existing coverage against the golden-test minimum in
`docs/protocol-normalization.md`:

- plain text, system prompts, tool calls, tool results
- streaming text and streaming tool calls
- stop reason and usage mapping
- provider-specific unsupported fields
- stream errors, malformed events, and disconnect behavior
- snapshot/golden coverage gaps

Do not fill all fixture gaps in this phase. Record the gaps so Phase 2, Phase
5, and Phase 12 add the right adapter fixtures.

### Route guardrails

Add or update route tests so they assert:

- `POST /v1/messages` with valid Anthropic JSON parses and validates, and any
  failure is not a bad-JSON/request-shape failure.
- `POST /v1/chat/completions` with OpenAI-shaped JSON remains unmounted until
  Phase 9.

### Completion criteria

- No production files changed in Phase 0.
- Characterization tests for the current transformer and route behavior exist.
- `POST /v1/chat/completions` remains unmounted.
- No new direct protocol-pair transformer is introduced.
- `cargo test --workspace` and clippy pass before Phase 1 starts.
