# Final Verification Gate

Run in order:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace
cargo build --workspace --locked --release
! rg -n "transformer|detect_scenario|route_for_streaming|FallbackHandler|OpenCodeClient|EndpointType|classify_endpoint|is_anthropic_model|is_gemini_model|is_responses_model|is_zen" crates apps
! rg -n "handle_openai_streaming|handle_responses_streaming|handle_gemini_streaming|spawn_proxy_task" crates/llm-proxy-server/src/routes
```

The negative `rg` checks enforce the architecture: no direct transformer path,
scenario/fallback chain, model-ID protocol classifier, or provider-specific
stream handler may remain in live code.

Manual smoke after `llm-proxy serve --config ./config.toml`:

```sh
curl -sS http://127.0.0.1:3456/health
curl -sS http://127.0.0.1:3456/ready
curl -sS http://127.0.0.1:3456/version
curl -sS -X POST http://127.0.0.1:3456/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"unknown","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
curl -sS -X POST http://127.0.0.1:3456/v1/messages/count_tokens \
  -H 'content-type: application/json' \
  -d '{"model":"any","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
curl -sS -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"unknown","messages":[{"role":"user","content":"hi"}]}'
llm-proxy validate --config ./config.toml
llm-proxy models --config ./config.toml
```

Manual smoke with configured mock/local providers:

```sh
curl -sS -X POST http://127.0.0.1:3456/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"configured-chat","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
curl -sS -N -X POST http://127.0.0.1:3456/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"configured-chat","max_tokens":16,"stream":true,"messages":[{"role":"user","content":"hi"}]}'
curl -sS -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"configured-chat","messages":[{"role":"user","content":"hi"}]}'
curl -sS -N -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"configured-chat","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

Streaming smoke assertions:

- Anthropic stream emits `message_start`, content delta events, usage where
  available, and terminal `message_stop`.
- OpenAI stream emits `chat.completion.chunk`, usage where available, and final
  `data: [DONE]`.
- Stream error paths use the route-specific error envelope.

Then stop the TOML server and run the old-JSON migration check as a separate
invocation:

```sh
llm-proxy serve --config ./old.json
```

Expected:

- health/ready/version return 200
- unknown Anthropic model returns 400 Anthropic-shaped error
- token count returns 200 and does not call upstream
- unknown OpenAI Chat model returns 400 OpenAI-shaped error
- validate succeeds for TOML config
- models prints configured client model IDs
- old JSON serve exits non-zero with migration error
- no request performs scenario detection
- no request invokes fallback
- no model ID classifier decides protocol
- successful configured Anthropic and OpenAI Chat non-stream requests traverse
  `wire -> core -> wire`
- successful configured Anthropic and OpenAI Chat stream requests traverse
  provider decoder state and client stream encoders

## Implementation Order Summary

```text
0. Verify current green baseline.
1. Add CoreRequest/CoreResponse/CoreEvent.
2. Add Anthropic and OpenAI Chat client adapters plus client fixtures.
3. Add TOML model/provider config beside old JSON config.
4. Add protocol-neutral ProxyClient and SSE framer.
5. Add provider protocol adapters plus provider fixtures.
6. Add provider registry resolution.
7. Rewrite AppState.
8. Rewrite /v1/messages through core.
9. Mount real /v1/chat/completions through core.
10. Replace CLI config commands.
11. Delete old scenario/fallback/direct-transform code.
12. Complete golden fixture coverage.
```

The runtime architecture is complete only when Phase 11 is done. The migration
is not implementation-complete until Phase 12 verifies fixture coverage. Before
Phase 11, the workspace may contain compatibility code, but new route behavior
must use the core pipeline as soon as Phase 8 starts.

Back to parent plan: [`docs/plan.md`](../plan.md).
