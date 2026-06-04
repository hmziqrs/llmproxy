# Overview And Architecture

> **Status:** Active implementation plan for the next protocol/provider phase
> **Date:** 2026-06-05
> **Active path:** `docs/plan.md`
> **Standards:** `docs/protocol-mini.md`,
> `docs/protocol-normalization.md`
> **History:** Continues `docs/completed/initial-server-plan.md` and
> supersedes the old `docs/initial-server-plan-continuation.md` path.

## Purpose

The first server plan got the workspace and server running. The current code is
no longer the empty scaffold described at the beginning of that plan. It now has:

- JSON `oc-go-cc`-style config in `crates/llm-proxy-core/src/config.rs`.
- Scenario routing and fallback/circuit-breaker code under
  `crates/llm-proxy-core/src/router/`.
- Anthropic, OpenAI Chat Completions, Responses, and Gemini wire types in
  `crates/llm-proxy-protocol/src/`.
- Direct Anthropic-to-provider transformers in
  `crates/llm-proxy-protocol/src/transformer/`.
- `OpenCodeClient` with model-ID endpoint classification in
  `crates/llm-proxy-provider/src/client.rs`.
- `/v1/messages` wired in `crates/llm-proxy-server/src/routes/messages.rs`.
- An OpenAI echo route file at `crates/llm-proxy-server/src/routes/chat.rs`,
  but it is not mounted in `routes/mod.rs`.
- Metrics, rate limiting, request deduplication, request IDs, token counting,
  daemon/PID handling, and CLI commands already implemented.

The next phase is not "make providers configurable" in isolation. The next
phase is to make the live proxy follow the protocol-normalized architecture:

```text
client wire protocol -> CoreRequest/CoreResponse/CoreEvent -> provider wire protocol
```

Provider configurability lands inside that architecture, not beside it.

This plan uses the local names `CoreRequest`, `CoreResponse`, and `CoreEvent`
for the v1 chat-family core. Those are the same architectural layer called
`CoreChat` and `CoreChatStream` in `docs/protocol-mini.md`; they are not a
generic core for embeddings, images, audio, rerank, files, or batch endpoints.
Future endpoint families must get their own core contracts.

## Hard Rules

These rules prevent the plan from drifting into another direct pairwise
transform design.

1. No direct protocol pairs:

   ```text
   OpenAI Chat -> Anthropic Messages
   Anthropic Messages -> OpenAI Chat
   OpenAI Responses -> Anthropic Messages
   Gemini GenerateContent -> OpenAI Chat
   Gemini GenerateContent -> Anthropic Messages
   ```

   Every conversion must be:

   ```text
   client wire -> chat-family core -> provider wire
   ```

2. The router only selects:

   ```text
   requested model -> provider + upstream model
   ```

   It must not choose protocol families, build URLs, mutate sampling options, or
   inspect message content.

3. Provider adapters own provider wire details:

   - endpoint URL shape
   - request JSON shape
   - response JSON shape
   - streaming chunk parsing
   - provider-specific compatibility behavior

4. Client adapters own client wire details:

   - route request JSON to core
   - core response to route response JSON
   - core event stream to route SSE/chunks
   - route-specific error envelope

5. Client intent is preserved in `CoreRequest`.

   `temperature`, `top_p`, `max_tokens`, tools, tool choice, metadata,
   reasoning/thinking, cache hints, and `stream` come from the client request.
   Provider adapters may translate or omit unsupported fields, but the router and
   config do not override them.

6. A provider can be added with TOML only when it uses an already implemented
   provider protocol adapter. A new wire protocol requires code and fixtures.

7. Each phase must leave the workspace compiling and tests passing.

8. The generic paths in `docs/protocol-normalization.md` map to this repo's
   crate layout:

   ```text
   protocol/<client>.rs -> crates/llm-proxy-protocol/src/client/<client>.rs
   provider/<protocol>.rs -> crates/llm-proxy-provider/src/adapter/<protocol>.rs
   ```

   Only truly new provider wire formats need additional wire DTO modules.

## Target Crate Boundaries

### `llm-proxy-core`

Owns config, routing data, metrics, PID, and token counting.

Keep:

- `error.rs`
- `metrics.rs`
- `pid.rs`
- `token/`

Replace later:

- `config.rs` old JSON `Config`, `ModelConfig`, `OpenCodeGoConfig`,
  `OpenCodeZenConfig`
- `router/` scenario/fallback routing

Add:

- provider/model TOML config types
- provider registry validation
- model routing table lookup

### `llm-proxy-protocol`

Owns wire DTOs and normalized protocol types.

Keep as wire modules:

- `anthropic.rs`
- `openai.rs`
- `zen.rs`

Add:

- `core.rs`
- `client/mod.rs`
- `client/anthropic.rs`
- `client/openai_chat.rs`

Replace later:

- `transformer/request.rs`
- `transformer/response.rs`
- `transformer/stream.rs`

The replacement is not one huge file. It is adapters:

```text
Anthropic Messages <-> CoreRequest/CoreResponse/CoreEvent
OpenAI Chat        <-> CoreRequest/CoreResponse/CoreEvent
```

Provider-side protocol adapters live in `llm-proxy-provider`, but may reuse wire
DTOs from this crate.

### `llm-proxy-provider`

Owns upstream HTTP transport and provider protocol adapters.

Replace:

- `OpenCodeClient`
- `EndpointType`
- `classify_endpoint`
- `is_anthropic_model`
- `is_gemini_model`
- `is_responses_model`
- hardcoded OpenCode endpoint resolution

Add:

- protocol-neutral `ProxyClient`
- provider adapter trait/enum dispatch
- provider adapter registry
- OpenAI Chat Completions provider adapter
- Anthropic Messages provider adapter
- OpenAI Responses provider adapter
- Gemini GenerateContent provider adapter

### `llm-proxy-server`

Owns HTTP routes and application state.

Keep:

- middleware
- shutdown
- token count route initially
- metrics recording
- request ID generation
- rate limiting and request deduplication

Replace:

- `ModelRouter` in `state.rs`
- `fallback_handler` state
- scenario detection in `routes/messages.rs`
- endpoint classification dispatch in `routes/messages.rs`
- route-specific direct transformers
- circuit-breaker output in `routes/health.rs`

Add/mount:

- real `/v1/chat/completions` route using the same core pipeline
- route pipeline helper shared by Anthropic and OpenAI Chat routes

### `apps/llm-proxy`

Owns CLI, process lifecycle, and config file commands.

Replace gradually:

- JSON default config string
- hardcoded model list
- `OC_GO_CC_CONFIG` as the only env var
- validation output that assumes scenarios/fallbacks/OpenCode-only providers

Add:

- TOML config generation
- provider file generation
- provider adapter validation
- model table listing from config

## Target Runtime Pipeline

### Non-Streaming

```text
HTTP route
  -> parse client request JSON
  -> client adapter decode_request(...)
  -> CoreRequest
  -> routing table lookup by CoreRequest.model.requested
  -> ProviderTarget { provider, requested_model, upstream_model }
  -> provider registry resolve adapter by provider-local model table
  -> provider adapter encode_request(...)
  -> ProxyClient send(...)
  -> provider adapter decode_response(...)
  -> CoreResponse
  -> client adapter encode_response(...)
  -> HTTP JSON response
```

### Streaming

```text
HTTP route
  -> parse client request JSON
  -> client adapter decode_request(...)
  -> CoreRequest { stream: true, ... }
  -> routing table lookup
  -> provider registry resolve adapter
  -> provider adapter encode_request(...)
  -> ProxyClient send_stream(...)
  -> provider adapter decode stream into CoreEvent values
  -> client adapter encode CoreEvent values into route SSE/chunks
  -> HTTP stream response
```

Provider stream state stays in provider adapters. Client stream state stays in
client adapters. No route should parse OpenAI chunks and emit Anthropic events
directly.

Back to parent plan: [`docs/plan.md`](../plan.md).
