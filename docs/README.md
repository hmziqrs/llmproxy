# Planning notes

Design exploration for the `llm-proxy` engine. Not a spec. Not a
commitment. Captures the shape of the problem and the decisions still
open.

The crate layout in the root `README.md` is the working scaffold:

```
apps/llm-proxy                 main binary
crates/llm-proxy-core          config, types, traits
crates/llm-proxy-protocol      OpenAI / Claude / Gemini wire types
crates/llm-proxy-provider      upstream channel implementations
crates/llm-proxy-storage       persistence
crates/llm-proxy-api           admin and user HTTP API
crates/llm-proxy-server        axum wiring
```

The docs in this directory map onto those crates.

## Documents

| File | Scope |
|---|---|
| [protocol-normalization.md](protocol-normalization.md) | Focused rules for `A -> Core -> B` protocol normalization |
| [protocol-mini.md](protocol-mini.md) | Short reference-project findings on protocol families and response shapes |
| [quirks.md](quirks.md) | Flat catalogue of gotchas and broken things in the reference projects |
| [mini-quirks.md](mini-quirks.md) | Practical hacks and design rules distilled from the reference projects |

## Status of the reference projects

`ref/` contains four projects studied while scoping this:

| Project | Lang | Use as |
|---|---|---|
| `oc-go-cc` | Go | Reference for a working Anthropic proxy, but it is hardcoded to OpenCode Go/Zen. Reusable: the transformer and streaming code. Not reusable: the router, the endpoint classifier, the model list. |
| `llm-api-key-proxy` | Python | Key-rotation proxy, different product. |
| `llm-proxy` | Python | A different LLM proxy. |
| `litellm` | Python | The 100+ provider reference. Demonstrates the canonical IR + 2N converters pattern in production. |
