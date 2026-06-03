# Mini Quirks

Practical hacks mature proxies use because provider protocols do not line up cleanly.
Findings from deep passes over the reference projects.

> CoreChatStream is not passthrough. CoreChatStream is a state machine.

Split into themed chunks under [`quirks/`](quirks/):

- [Core Principles](quirks/01-principles.md) — 11 sections
- [Streaming & Stream Events](quirks/02-streaming.md) — 20 quirks
- [Tool Calls & Tool Schemas](quirks/03-tool-calls.md) — 19 quirks
- [Usage, Cost & Quota](quirks/04-usage-cost-quota.md) — 13 quirks
- [Routing, Fallback & Lifecycle](quirks/05-routing-lifecycle.md) — 25 quirks
- [Provider Quirks: Anthropic](quirks/06-providers-anthropic.md) — 13 quirks
- [Provider Quirks: Gemini / Vertex](quirks/07-providers-gemini-vertex.md) — 5 quirks
- [Provider Quirks: Azure](quirks/08-providers-azure.md) — 5 quirks
- [Provider Quirks: AWS / Bedrock](quirks/09-providers-aws-bedrock.md) — 7 quirks
- Provider Quirks: OpenAI-Compatible & Misc — 56 quirks, split:
  - [Part 1](quirks/10-providers-misc-1.md)
  - [Part 2](quirks/10-providers-misc-2.md)
  - [Part 3](quirks/10-providers-misc-3.md)
- [Endpoints: Embeddings & Rerank](quirks/11-endpoints-embeddings-rerank.md) — 14 quirks
- [Endpoints: Realtime, Audio & TTS](quirks/12-endpoints-realtime-audio.md) — 23 quirks
- [Endpoints: Image & Video](quirks/13-endpoints-image-video.md) — 14 quirks
- [Endpoints: Files, OCR, Vector Stores & Batch](quirks/14-endpoints-files-ocr-batch.md) — 22 quirks
- [Search Bridges](quirks/15-search-bridges.md) — 11 quirks

