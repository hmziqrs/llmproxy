# Phase 12 - Complete Golden Fixture Coverage

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: audit and complete fixture coverage. Basic client fixtures must already
exist from Phase 2, and basic provider fixtures must already exist from Phase 5.
This phase closes gaps; it must not be the first time fixtures are added.

### Fixture layout

Add:

```text
crates/llm-proxy-protocol/tests/fixtures/anthropic/
crates/llm-proxy-protocol/tests/fixtures/openai_chat/
crates/llm-proxy-provider/tests/fixtures/openai_chat/
crates/llm-proxy-provider/tests/fixtures/anthropic/
crates/llm-proxy-provider/tests/fixtures/responses/
crates/llm-proxy-provider/tests/fixtures/gemini/
```

If these directories already exist from earlier phases, keep them and add only
the missing cases.

Each fixture case should have:

```text
input.json
core.json
output.json
```

For streams:

```text
input.sse
core-events.json
output.sse
```

### Minimum fixture cases

For every adapter:

- plain text
- system prompt
- multiple messages
- tool definition
- tool choice
- assistant tool call
- user tool result
- reasoning/thinking
- cache marker where supported
- stop reason mapping
- stop sequence mapping where supported
- usage mapping
- streaming text
- streaming tool call
- malformed/unsupported provider field

### Dependency choice

Use plain JSON fixture comparison first. Add `insta` only if fixture updates
become tedious.

If adding `insta`, add it as a dev dependency only:

```toml
[dev-dependencies]
insta = { version = "1", features = ["json"] }
```

### Gate

```sh
cargo test --workspace
```
