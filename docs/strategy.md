# Strategy

Open questions and the real risk.

## Open strategic questions

These change the shape of the whole project. Worth deciding before
designing further.

### Who is the end user?

- Just you
- A small team
- A SaaS

Single-tenant: ignore multi-tenancy for a year. SaaS: cannot ignore it.

### What client faces the proxy?

- Only Claude Code
- Only Codex
- Both
- "Any OpenAI or Anthropic client"

This determines which wire format is the "default inbound" and how
complete the other side has to be. A proxy that only fronts Claude
Code with Anthropic-format inbound is a different product from one
that fronts arbitrary clients with both formats inbound.

### Hosting model?

- Local-only (sidecar to Claude Code / Codex)
- Shared endpoint (LAN / public)

Local-only: no auth, no multi-tenant work. Shared endpoint is a
different product with a different ops surface.

### Local models in scope?

Ollama, vLLM, LM Studio, llama.cpp server. They are OpenAI-compat so
wiring is cheap, but routing decisions get more interesting: not every
local model can do tools, vision, or extended thinking. A capability
matrix per model is needed.

### How much of LiteLLM do you actually want?

LiteLLM has 100+ providers but its IR is "whatever Python dict
happens to work." A clean IR plus 5-10 first-class providers is a
better product than a 100-provider mess. The question is where on that
curve this project sits.

## Scope discipline

The single hardest thing is feature creep disguised as small asks.

"Just add Cohere" is a bespoke wire format and a maintenance debt.
"Just add a semantic cache" is an embedding provider, a vector store,
and another config surface. "Just add a Postgres backend" is a
storage abstraction and a migration story. Every feature in
[operations.md](operations.md) is worth building eventually. Almost
none belong in v1.

The line:

- v1 builds the engine so it can convert and stream correctly.
- v2+ builds the operations surface that production needs.

Conflating the two produces a product that is bad at both.

## The real risk

Not the IR, not streaming. Those are engineering and have known
solutions. The risk is that the IR bends to accommodate the 11th
provider's quirk and stops being canonical. Once that happens, every
new provider is N work again and the engine has degraded into a soup
of special cases.

The discipline that prevents it: **the IR is a hard contract**. A
provider that needs a field the IR does not have either:

- gets the field added to the IR (with justification), or
- loses the field on translation (with a warning in the response),
  or
- goes into `provider_meta` as opaque JSON (the escape hatch).

The first option is rare. The second is common and acceptable. The
third is the pressure-release valve. None of them is "we'll just
special-case it in this provider's client.rs."

## The fixtures problem

Translation bugs are silent. Claude Code's auto-compact or a tool-call
loop is where they show up, weeks after the change. The only defense
is a large corpus of captured real requests/responses stored as
`tests/fixtures/*.json` with snapshot tests on the canonical output.

Capture a few hundred real exchanges during the first round-trip work.
Store them in version control. Re-run on every change. This is the
single highest-leverage piece of test infrastructure in the project.

## The streaming problem

Streaming is where most proxy projects ship something that mostly
works and breaks in production on the 0.1% of requests that have
multiple parallel tool calls, mid-stream refusals, or a model that
emits an unknown event type. Same discipline applies: capture real
streams, store them, replay them in tests, byte-for-byte event
ordering assertions.

The Go reference (`ref/oc-go-cc/internal/transformer/stream.go`) is
969 lines. Most of that is the state machine for tool calls and
reasoning content. Plan for a similar amount of careful work in the
Rust port, with the same kind of "fast path" string scanning to
preserve TTFT.

## What to defer

In rough order of how tempting each one is and how bad the deferral
cost actually is:

| Tempting | Real cost of deferral |
|---|---|
| Semantic cache | Low. Exact-match cache is enough for v1. |
| Postgres storage backend | Low. SQLite or even JSON files cover v1. |
| Multi-tenant rate limits | Low if you are single-tenant. |
| OTLP export | Low. Structured logs are enough. |
| Bedrock SigV4 | Medium. Real if any target user needs AWS. |
| Vertex service-account auth | Medium. Same. |
| Web UI / admin dashboard | Low. CLI is fine for v1. |
| TUI | Low. None. |
