# Test Inventory -- Recorded Fixture Gaps

> Parent plan: [`docs/plan-phases/phase-00-current-state-guardrails.md`](phase-00-current-state-guardrails.md)
> Standards: [`docs/protocol-normalization.md`](../protocol-normalization.md)

This document records known test coverage gaps identified during the Phase 0
baseline audit. Per the plan: _"Do not fill all fixture gaps in this phase.
Record the gaps so Phase 2, Phase 5, and Phase 12 add the right adapter fixtures."_

## Streaming gaps

1. **Streaming tool calls for Responses API**: `process_responses_chunk` does
   not handle `response.function_call_arguments.delta` events. Text deltas
   work, but function call deltas are silently ignored.

2. **Streaming function calls for Gemini**: `process_gemini_chunk` only handles
   text parts, not function call parts in the Gemini response.

3. **Upstream stream errors**: No test simulates an HTTP 500 mid-stream from
   the upstream provider. The current code handles this by breaking out of the
   stream loop in `spawn_proxy_task` (`Err(_) => break`), but this path is
   untested.

4. **Client disconnect mid-stream**: No test simulates an actual client
   disconnect (dropping the response body while SSE events are being written).
   `ErrClientDisconnected` exists but is only tested for its `Display`/`Debug`
   formatting.

5. **Disconnect behavior for Responses/Gemini streams**: Same as above but
   specific to the Responses API and Gemini streaming paths.

## Content type gaps

6. **Image content blocks**: Images are replaced with `[Image]` placeholders in
   the transformer. Full image passthrough requires core protocol support.

7. **Redacted thinking blocks**: The `redacted_thinking` content type is not
   modeled in the transformer.

8. **Refusal content types**: OpenAI `refusal` fields on messages are not
   forwarded through the transformer.

## Fixture/snapshot gaps

9. **No snapshot/golden fixtures**: No snapshot tests exist for any adapter.
   These should be added when the core protocol architecture is in place, per
   the golden-test minimum in `docs/protocol-normalization.md` Section 10.

## Missing edge case tests

10. **Oversized request body**: No test for a POST body exceeding the 32 MiB
    `MAX_BODY_BYTES` limit. Axum's `DefaultBodyLimit` should return a 413, but
    this is untested.

11. **Unrecognized finish reasons in Gemini**: If Gemini returns a finish
    reason other than `"MAX_TOKENS"` or `"STOP"`, it falls through to the
    default `"end_turn"`. No tracing is emitted for unrecognized finish reasons
    in any of the three stream processors.

## Phase assignments

| Gap | Phase to fill |
|-----|---------------|
| 1, 2 | Phase 2 (core protocol streaming) |
| 3, 4, 5 | Phase 5 (streaming adapters) |
| 6, 7, 8 | Phase 2 (core protocol content types) |
| 9 | Phase 2, 5, 12 (adapter fixture infrastructure) |
| 10 | Phase 0 (can be added now) |
| 11 | Phase 5 (streaming adapters) |
