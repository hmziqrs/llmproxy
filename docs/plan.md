# Protocol Normalization Implementation Plan

> **Status:** Active implementation plan for the next protocol/provider phase
> **Date:** 2026-06-05
> **Active path:** `docs/plan.md`
> **Standards:** `docs/protocol-mini.md`, `docs/protocol-normalization.md`
> **History:** Continues `docs/completed/initial-server-plan.md` and supersedes
> the old `docs/initial-server-plan-continuation.md` path.

## Purpose

This file is the short source-of-truth index. The detailed plan is split into
phase files under `docs/plan-phases/` so each implementation step is small
enough to read and execute independently.

The target architecture remains the protocol-normalized path required by the
standards docs:

```text
client wire protocol -> CoreRequest/CoreResponse/CoreEvent -> provider wire protocol
```

`CoreRequest`, `CoreResponse`, and `CoreEvent` are the v1 chat-family core names
for the same layer called `CoreChat` and `CoreChatStream` in
`docs/protocol-mini.md`. They are not a generic core for embeddings, images,
audio, rerank, files, or batch endpoints.

## Read First

- [`docs/plan-phases/overview.md`](plan-phases/overview.md): detailed purpose,
  hard rules, crate boundaries, and target runtime pipeline.
- [`docs/protocol-mini.md`](protocol-mini.md): concise protocol family findings.
- [`docs/protocol-normalization.md`](protocol-normalization.md): normalization
  rules and adapter responsibilities.

## Non-Negotiable Rules

1. No direct protocol pairs. All translation is `client wire -> core -> provider wire`.
2. The router only maps requested model to provider and upstream model.
3. Client adapters own client wire JSON/SSE; provider adapters own provider wire JSON/SSE.
4. Client intent in `CoreRequest` is preserved; router and config do not override sampling or tool behavior.
5. TOML-only provider additions are allowed only when the provider uses an already implemented protocol adapter.
6. Every adapter gets fixtures, including stream fixtures, before the migration is complete.
7. Each phase must leave the workspace compiling and tests passing.

## Phase Index

| Phase | Goal | File |
|---:|---|---|
| 0 | Current-State Guardrails | [`plan-phases/phase-00-current-state-guardrails.md`](plan-phases/phase-00-current-state-guardrails.md) |
| 1 | Add Core Protocol Types | [`plan-phases/phase-01-add-core-protocol-types.md`](plan-phases/phase-01-add-core-protocol-types.md) |
| 2 | Add Client Protocol Adapters | [`plan-phases/phase-02-add-client-protocol-adapters.md`](plan-phases/phase-02-add-client-protocol-adapters.md) |
| 3 | Add Provider Config And Routing Types | [`plan-phases/phase-03-add-provider-config-and-routing-types.md`](plan-phases/phase-03-add-provider-config-and-routing-types.md) |
| 4 | Add Protocol-Neutral Transport | [`plan-phases/phase-04-add-protocol-neutral-transport.md`](plan-phases/phase-04-add-protocol-neutral-transport.md) |
| 5 | Add Provider Protocol Adapters | [`plan-phases/phase-05-add-provider-protocol-adapters.md`](plan-phases/phase-05-add-provider-protocol-adapters.md) |
| 6 | Build Provider Registry Resolution | [`plan-phases/phase-06-build-provider-registry-resolution.md`](plan-phases/phase-06-build-provider-registry-resolution.md) |
| 7 | Rewrite AppState | [`plan-phases/phase-07-rewrite-appstate.md`](plan-phases/phase-07-rewrite-appstate.md) |
| 8 | Rewrite `/v1/messages` | [`plan-phases/phase-08-rewrite-v1-messages.md`](plan-phases/phase-08-rewrite-v1-messages.md) |
| 9 | Mount Real `/v1/chat/completions` | [`plan-phases/phase-09-mount-real-v1-chat-completions.md`](plan-phases/phase-09-mount-real-v1-chat-completions.md) |
| 10 | Replace CLI Config Commands | [`plan-phases/phase-10-replace-cli-config-commands.md`](plan-phases/phase-10-replace-cli-config-commands.md) |
| 11 | Remove Old Direct Architecture | [`plan-phases/phase-11-remove-old-direct-architecture.md`](plan-phases/phase-11-remove-old-direct-architecture.md) |
| 12 | Complete Golden Fixture Coverage | [`plan-phases/phase-12-complete-golden-fixture-coverage.md`](plan-phases/phase-12-complete-golden-fixture-coverage.md) |

## Final Gate

- [`docs/plan-phases/final-verification.md`](plan-phases/final-verification.md)

## Supporting References

- [`docs/plan-phases/phase-03-provider-config-examples.md`](plan-phases/phase-03-provider-config-examples.md)

## Implementation Order

```text
0. Current-State Guardrails
1. Add Core Protocol Types
2. Add Client Protocol Adapters
3. Add Provider Config And Routing Types
4. Add Protocol-Neutral Transport
5. Add Provider Protocol Adapters
6. Build Provider Registry Resolution
7. Rewrite AppState
8. Rewrite `/v1/messages`
9. Mount Real `/v1/chat/completions`
10. Replace CLI Config Commands
11. Remove Old Direct Architecture
12. Complete Golden Fixture Coverage
```

The runtime architecture is complete only when Phase 11 is done. The migration
is not implementation-complete until Phase 12 verifies fixture coverage. Before
Phase 11, the workspace may contain compatibility code, but new route behavior
must use the core pipeline as soon as Phase 8 starts.
