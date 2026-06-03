# Operations

Cross-cutting concerns. None of these are v1 blockers, but the order
matters: cheaper to design in up front, expensive to retrofit.

## 1. Auth

Different providers, different schemes. Make this a `Provider`-supplied
object, not a config field.

| Scheme | Used by |
|---|---|
| Bearer | Most OpenAI-compat |
| `x-api-key` header | Anthropic native |
| SigV4 | AWS Bedrock |
| OAuth2 client-credentials | Vertex, some enterprise gateways |
| Query-param API key | Some smaller providers |
| Rotating keys (multiple, fail-over) | All, in production |

Lives in `crates/llm-proxy-core` (config) and
`crates/llm-proxy-provider` (auth strategy per provider).

## 2. Rate limits per upstream

Each provider has its own. Local 429/5xx triggers backoff. Cooldowns
persist across requests (in-memory per provider, or in
`crates/llm-proxy-storage` for cross-restart).

Two distinct concepts:

- **Per-provider limits** (the upstream's own quota). Backoff and retry.
- **Per-client limits** (this engine's quota to its own clients). The
  reverse direction.

Both are needed eventually. v1 likely needs only the first.

## 3. Retries

Non-streaming: idempotency keys plus bounded retry with exponential
backoff. Safe.

Streaming: partial streams plus resume is unsolved in general. **Plan
explicitly for "no retry on streams"** rather than discovering it.
Document the failure mode. Surface it to the client as an
`error` event.

## 4. Fallback chains

Primary to fallback to fallback, mixing providers per chain because
the IR is provider-agnostic:

```yaml
routes:
  - match: { model: "claude-opus-4" }
    chain:
      - anthropic/claude-opus-4-20250514
      - openai/gpt-4o
      - gemini/gemini-2.5-pro
```

A circuit breaker per upstream decides when to skip a chain element.
Lives in `crates/llm-proxy-core`. Same idea as
`ref/oc-go-cc/internal/router/fallback.go` (circuit breaker with 3
failures and 30s recovery), but provider-aware.

## 5. Cost tracking

Per-token pricing table, optional cost-cap on a request, optional
cost-based routing. Mostly config:

```yaml
models:
  - id: anthropic/claude-opus-4
    pricing: { input: 15, output: 75 }   # per 1M tokens USD
```

The "estimate cost before sending" hook matters for cost-based
routing. Compute it from the request IR without making the upstream
call.

## 6. Response caching

Two flavors:

- **Exact-match.** By (model, messages-hash, tools-hash). Straightforward.
  LRU plus TTL.
- **Semantic cache.** Embedding similarity. Needs an embedding provider
  itself, which means the engine has a chicken-and-egg dependency on
  itself. Skippable for v1.

Cache invalidation is a separate problem you do not want to discover
after launch. Default to short TTLs; never cache tool calls; never
cache refusals.

## 7. Observability

Two things to add on day one because they are free now and a nightmare
to retrofit:

- **Request IDs that propagate end-to-end.** Client-supplied or
  generated. Pass through to upstream via `X-Request-ID`. Echo back in
  response. Log them everywhere.
- **Structured logs.** Request id, provider, model, tokens, latency,
  cost. One event per request, plus per-upstream-call events.

Then later:

- **Metrics.** Per-provider latency p50/p95/p99, error rate, tokens
  in/out, cost accrued.
- **OTLP export.** For when you have a real OTel collector.
- **Request recording for replay.** Store sanitized request/response
  pairs to disk. Invaluable for debugging translation bugs.

Lives in `crates/llm-proxy-storage` (the on-disk side) and
`crates/llm-proxy-api` (the `/admin/metrics` style surface).

## 8. Multi-tenancy

Per-call API key selection, per-tenant rate limits, per-tenant budgets.
Skippable for personal use, required for SaaS. Defer until the
single-tenant path is stable.

## 9. Build phasing

Roughly in order of cost-of-deferral:

1. **v0.1** - Core engine: IR + registry + Anthropic & OpenAI Chat
   providers, both directions, no streaming. Proves the round-trip
   is faithful.
2. **v0.2** - Streaming: canonical event type, SSE parsers, renderers.
   Heartbeat, cancellation, error events.
3. **v0.3** - OpenAI Responses (Codex). The only big missing format.
   Tools, reasoning items, refusal.
4. **v0.4** - Generic OpenAI-compat provider. 50+ providers for free
   via config.
5. **v0.5** - Gemini, Bedrock, Vertex, Azure OpenAI.
6. **v0.6** - Fallback chains, circuit breakers, per-provider rate
   limits.
7. **v0.7** - Cost tracking, response caching (exact-match).
8. **v0.8** - Observability surface: metrics endpoint, OTLP.
9. **v1.0** - Multi-tenancy, semantic cache, request recording.

The discipline that makes this work: freeze the IR after v0.1. If
the IR survives the Anthropic-to-OpenAI round-trip with golden
fixtures, the rest is mechanical.
