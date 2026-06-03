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
| [architecture.md](architecture.md) | Engine shape, canonical IR, provider layer, streaming, extensibility story |
| [protocol-normalization.md](protocol-normalization.md) | Focused rules for `A -> Core -> B` protocol normalization |
| [protocol-mini.md](protocol-mini.md) | Short reference-project findings on protocol families and response shapes |
| [mini-quirks.md](mini-quirks.md) | Quirks and hacks found in reference proxy implementations |
| [operations.md](operations.md) | Cross-cutting concerns: auth, rate limits, retries, fallbacks, caching, observability, multi-tenancy |
| [strategy.md](strategy.md) | Open strategic questions, scope discipline, real risk |

## Status of the reference projects

`ref/` contains four projects studied while scoping this:

| Project | Lang | Use as |
|---|---|---|
| `oc-go-cc` | Go | Reference for a working Anthropic proxy, but it is hardcoded to OpenCode Go/Zen. Reusable: the transformer and streaming code. Not reusable: the router, the endpoint classifier, the model list. |
| `llm-api-key-proxy` | Python | Key-rotation proxy, different product. |
| `llm-proxy` | Python | A different LLM proxy. |
| `litellm` | Python | The 100+ provider reference. Demonstrates the canonical IR + 2N converters pattern in production. |
