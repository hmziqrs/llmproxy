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

Interpret fixture names by adapter direction:

- client request decode: `input.json` is client wire request, `core.json` is
  `CoreRequest`
- client response encode: `core.json` is `CoreResponse`, `output.json` is
  client wire response
- client stream encode: `core-events.json` is `CoreEvent[]`, `output.sse` is
  client stream output
- provider request encode: `core.json` is `CoreRequest`, `output.json` is
  provider wire request
- provider response decode: `input.json` is provider wire response, `core.json`
  is `CoreResponse`
- provider stream decode: `input.sse` is provider stream input,
  `core-events.json` is `CoreEvent[]`

Add a fixture manifest or coverage-matrix test that enumerates every adapter,
direction, and required case. `cargo test` must fail when a required fixture is
missing.

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
- redacted thinking
- refusal
- image/document/audio/video content, either preserved where supported or
  intentionally rejected/warned
- cache marker where supported
- sampling and intent preservation: temperature, top-p, max tokens, stream,
  caller metadata, provider hints/raw fields
- model mapping: client adapters preserve `CoreRequest.model.requested`;
  provider adapters encode `target.upstream_model`, including Gemini URL
  template expansion
- stop reason mapping
- stop sequence mapping where supported
- usage mapping
- streaming text
- streaming tool call
- streaming thinking deltas
- streaming usage deltas
- terminal stop reason and stop sequence
- ping and error events
- unknown provider events
- malformed frames
- partial tool-call JSON buffering
- upstream disconnect handling
- malformed client request fields
- unsupported core content on client encode
- malformed provider response/SSE
- unsupported provider fields
- lossy translation warning/provider_meta behavior

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
