# Research Notes

These notes summarize compatibility lessons from the projects under `ref/`.
They are design input, not a claim that every provider or endpoint is already
implemented.

The current proxy exposes OpenAI Chat Completions and Anthropic Messages client
routes. Upstream adapters support OpenAI Chat Completions, Anthropic Messages,
OpenAI Responses, and Gemini GenerateContent. The notes also cover useful
extension points for native providers and additional endpoint families.

## Contents

- [Core protocol and lifecycle](core-protocol.md) — normalization, streaming,
  tools, usage, routing, retries, and shutdown.
- [Provider compatibility](providers.md) — provider-specific authentication,
  request shaping, response repair, and stable OpenCode configuration.
- [Extended endpoints](endpoints.md) — embeddings, rerank, realtime, audio,
  images, video, files, OCR, batch, vector stores, and search.

The normative protocol direction remains in
[`protocol.md`](../protocol.md). This directory records edge cases that should
influence implementations and tests.

## How to use these notes

When adding a protocol or provider:

1. Add or extend an endpoint-family core instead of translating one provider
   directly into another.
2. Treat streaming as a state machine, not a sequence of independent chunks.
3. Make lossy translation, parameter dropping, and synthetic data observable.
4. Keep provider quirks at the adapter or transport edge.
5. Add fixtures for malformed, partial, streaming, tool-call, and usage cases.

Provider catalogs and prices are intentionally not copied here. They change
too frequently; use live discovery or maintained provider configuration
instead.
