# OpenCode Go & Zen — API & Model Metadata Reference

Detailed reference for the OpenCode Go and Zen plan APIs, model catalogs,
metadata sources, and configuration templates. Intended as a lookup document
for building discovery scripts, local configurations, and provider catalog
files.

All data was collected on 2026-06-08/09 via live API calls and official
documentation.

---

## 1. Overview

OpenCode (https://opencode.ai) offers two plans exposed as OpenAI-compatible
API gateways:

- **OpenCode Go** — Chinese-model-focused plan. 18 models from GLM, Kimi,
  MiMo, Qwen, MiniMax, DeepSeek, and Tencent.
- **OpenCode Zen** — Full-catalog plan. 46 models including everything from
  Go plus Anthropic Claude, OpenAI GPT, Google Gemini, xAI Grok, NVIDIA
  Nemotron, and free-tier promotional models.

Both plans are accessed through subpath-based base URLs under `opencode.ai`:

```text
Go:  https://opencode.ai/zen/go/v1/...
Zen: https://opencode.ai/zen/v1/...
```

Our proxy uses these as provider backends. Each plan maps to a single
`[provider]` TOML block with adapters for each protocol family and routes
that select which adapter handles which inbound request shape.

---

## 2. API Endpoints

### 2.1 Model Listing

```text
GET https://opencode.ai/zen/go/v1/models   → 18 models (Go plan)
GET https://opencode.ai/zen/v1/models      → 46 models (Zen plan)
```

- No authentication required.
- Returns standard OpenAI `/v1/models` shape.
- **No per-model detail endpoint**: `GET /v1/models/{model_id}` returns 404
  (HTML, not JSON).

### 2.2 Inference Endpoints

```text
# Go plan
POST https://opencode.ai/zen/go/v1/chat/completions
POST https://opencode.ai/zen/go/v1/messages

# Zen plan
POST https://opencode.ai/zen/v1/chat/completions
POST https://opencode.ai/zen/v1/messages
POST https://opencode.ai/zen/v1/responses
POST https://opencode.ai/zen/v1/models/{model}:generateContent
```

- Auth: `Authorization: Bearer <key>` for all endpoints.
- The Zen plan exposes two additional adapters: OpenAI Responses API and
  Gemini-native generateContent.

### 2.3 Endpoint Summary Table

| Adapter | Protocol | Go URL | Zen URL |
|---------|----------|--------|---------|
| `chat` | `openai_chat_completions` | `/zen/go/v1/chat/completions` | `/zen/v1/chat/completions` |
| `anthropic` | `anthropic_messages` | `/zen/go/v1/messages` | `/zen/v1/messages` |
| `responses` | `openai_responses` | — | `/zen/v1/responses` |
| `gemini` | `gemini_generate_content` | — | `/zen/v1/models/{model}:generateContent` |

---

## 3. `/v1/models` Response Shape

The model listing endpoints return minimal OpenAI-style records with **no**
metadata about context limits, capabilities, or pricing.

```json
{
  "object": "list",
  "data": [
    {
      "id": "claude-sonnet-4-6",
      "object": "model",
      "created": 1781011253,
      "owned_by": "opencode"
    }
  ]
}
```

Every model shares the same `created` timestamp and `owned_by: "opencode"`.
The only useful field is `id`.

To get metadata (context limits, reasoning support, tool calling, costs),
use the models.dev catalog (Section 5).

---

## 4. Complete Model Lists

### 4.1 Go Plan — 18 Models

All fields sourced from `https://models.dev/models.json` unless marked with
`*` (inferred from provider documentation).

| Model ID | Provider | Context | Output | Reason | Tools | Temp | Attach | Modalities In |
|----------|----------|---------|--------|--------|-------|------|--------|---------------|
| `glm-5.1` | Zhipu | 200K | 131K | ✅ | ✅ | ✅ | ❌ | text |
| `glm-5` | Zhipu | 205K | 131K | ✅ | ✅ | ✅ | ❌ | text |
| `kimi-k2.6` | Moonshot | 262K | 262K | ✅ | ✅ | ✅ | ✅ | text, image, video |
| `kimi-k2.5` | Moonshot | 262K | 262K | ✅ | ✅ | ❌ | ❌ | text, image, video |
| `mimo-v2.5-pro` | Xiaomi | 1M | 131K | ✅ | ✅ | ✅ | ❌ | text |
| `mimo-v2.5` | Xiaomi | 1M | 131K | ✅ | ✅ | ✅ | ✅ | text, image, audio, video |
| `mimo-v2-pro` | Xiaomi | 1M | 131K | ✅ | ✅ | ✅ | ❌ | text |
| `mimo-v2-omni` | Xiaomi | 262K | 131K | ✅ | ✅ | ✅ | ✅ | text, image, audio, video, pdf |
| `qwen3.7-max` | Alibaba | 1M | 65K | ✅ | ✅ | ✅ | ❌ | text |
| `qwen3.7-plus` | Alibaba | 1M | 64K | ✅ | ✅ | ✅ | ❌ | text, image |
| `qwen3.6-plus` | Alibaba | 1M | 65K | ✅ | ✅ | ✅ | ❌ | text, image, video |
| `qwen3.5-plus` | Alibaba | 1M | 65K | ✅ | ✅ | ✅ | ❌ | text, image, video |
| `minimax-m3` | MiniMax | 1M* | —* | —* | —* | —* | —* | —* |
| `minimax-m2.7` | MiniMax | 1M* | —* | —* | —* | —* | —* | —* |
| `minimax-m2.5` | MiniMax | 1M* | —* | —* | —* | —* | —* | —* |
| `deepseek-v4-pro` | DeepSeek | 1M | 384K | ✅ | ✅ | ✅ | ❌ | text |
| `deepseek-v4-flash` | DeepSeek | 1M | 384K | ✅ | ✅ | ✅ | ❌ | text |
| `hy3-preview` | Tencent | 256K | 64K | ✅ | ✅ | ✅ | ❌ | text |

\* MiniMax models are not listed in models.dev. Context window estimated at 1M
based on M-series documentation. Detailed API specs not publicly available
(provider site is a JavaScript SPA).

### 4.2 Zen Plan — 46 Models

#### Anthropic Claude (9 models)

All use the Anthropic Messages API (`/v1/messages`). All support extended
thinking.

| Model ID | Context | Output | Reasoning | Temp | Attach | Knowledge Cutoff | Released |
|----------|---------|--------|-----------|------|--------|-------------------|----------|
| `claude-opus-4-8` | 1M | 128K | adaptive | ❌ | ✅ | — | 2026-05-28 |
| `claude-opus-4-7` | 1M | 128K | extended | ❌ | ✅ | 2026-01 | 2026-04-16 |
| `claude-opus-4-6` | 1M | 128K | extended | ✅ | ✅ | 2025-05 | 2026-02-05 |
| `claude-opus-4-5` | 200K | 64K | extended | ✅ | ✅ | 2025-03 | 2025-11-24 |
| `claude-opus-4-1` | 200K | 32K | extended | ✅ | ✅ | 2025-03 | 2025-08-05 |
| `claude-sonnet-4-6` | 1M | 64K | extended+adaptive | ✅ | ✅ | 2025-08 | 2026-02-17 |
| `claude-sonnet-4-5` | 200K | 64K | extended | ✅ | ✅ | 2025-07 | 2025-09-29 |
| `claude-sonnet-4` | 200K | 64K | extended | ✅ | ✅ | — | — |
| `claude-haiku-4-5` | 200K | 64K | extended | ✅ | ✅ | 2025-02 | 2025-10-15 |

Notes:

- Opus 4.7+ and Sonnet 4.6+ have 1M context. Earlier models are 200K.
- Opus 4.8 uses adaptive thinking only (no explicit extended thinking toggle).
- Sonnet 4.6 supports both extended and adaptive thinking.
- All support tool calling, attachments (text, image, pdf input), and text output.
- Batch API can extend output to 300K for Opus 4.6+ and Sonnet 4.6.

#### OpenAI GPT (17 models)

All use the OpenAI Chat Completions API. The `gpt-5.3-codex-spark` model ID
is present in the Zen `/v1/models` list but not in models.dev.

| Model ID | Context | Input Limit | Output | Reasoning | Temp | Attach | PDF | Released |
|----------|---------|-------------|--------|-----------|------|--------|-----|----------|
| `gpt-5.5` | 1.05M | 922K | 128K | ✅ | ❌ | ✅ | ✅ | 2026-04-23 |
| `gpt-5.5-pro` | 1.05M | 922K | 128K | ✅ | ❌ | ✅ | ✅ | 2026-04-23 |
| `gpt-5.4` | 1.05M | 922K | 128K | ✅ | ❌ | ✅ | ✅ | 2026-03-05 |
| `gpt-5.4-pro` | 1.05M | 922K | 128K | ✅ | ❌ | ✅ | ❌ | 2026-03-05 |
| `gpt-5.4-mini` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2026-03-17 |
| `gpt-5.4-nano` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2026-03-17 |
| `gpt-5.3-codex` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ✅ | 2026-02-05 |
| `gpt-5.3-codex-spark` | — | — | — | — | — | — | — | — |
| `gpt-5.2` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-12-11 |
| `gpt-5.2-codex` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ✅ | 2025-12-11 |
| `gpt-5.1` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-11-13 |
| `gpt-5.1-codex-max` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-11-13 |
| `gpt-5.1-codex` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-11-13 |
| `gpt-5.1-codex-mini` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-11-13 |
| `gpt-5` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-08-07 |
| `gpt-5-codex` | 400K | 272K | 128K | ✅ | ❌ | ❌ | ❌ | 2025-09-15 |
| `gpt-5-nano` | 400K | 272K | 128K | ✅ | ❌ | ✅ | ❌ | 2025-08-07 |

Notes:

- GPT-5.4+ (flagship) has 1.05M context with 922K input limit.
- GPT-5.0–5.3 have 400K context with 272K input limit.
- All have 128K max output.
- None support temperature parameter (reasoning models).
- `gpt-5.3-codex-spark` is listed in Zen but has no models.dev entry.

#### Google Gemini (3 models)

All use the Gemini native API (`generateContent` endpoint). All have 1M
context and 65K output.

| Model ID | Context | Output | Reasoning | Temp | Modalities In | Status |
|----------|---------|--------|-----------|------|---------------|--------|
| `gemini-3.5-flash` | 1M | 65K | ✅ (thinkingLevel) | ✅ | text, image, video, audio, pdf | Stable |
| `gemini-3.1-pro` | 1M | 65K | ✅ (always on) | ✅ | text, image, video, audio, pdf | Preview |
| `gemini-3-flash` | 1M | 65K | ✅ (thinkingLevel) | ✅ | text, image, video, audio, pdf | Shut down* |

\* `gemini-3-flash` is listed under "Previous models" as "Shut down" on
Google's models page. The stable replacement is `gemini-3.5-flash`.

#### xAI (1 model)

| Model ID | Context | Output | Reasoning | Tools | Attach | Modalities In |
|----------|---------|--------|-----------|-------|--------|---------------|
| `grok-build-0.1` | 256K | 256K | ✅ | ✅ | ✅ | text, image, pdf |

Not listed in xAI's public model catalog at `https://docs.x.ai/docs/models`.
Likely a Zen-exclusive build/code-optimized variant. Priced at $1.00/$2.00
per 1M tokens (input/output) on Zen.

#### Shared Models (also in Go)

The following models appear in both Go and Zen plans with identical specs:

```text
deepseek-v4-flash, deepseek-v4-pro, glm-5, glm-5.1,
kimi-k2.5, kimi-k2.6, minimax-m2.5, minimax-m2.7, minimax-m3,
mimo-v2-pro, mimo-v2-omni, mimo-v2.5, mimo-v2.5-pro,
qwen3.5-plus, qwen3.6-plus, qwen3.7-max, qwen3.7-plus, hy3-preview
```

See Section 4.1 for their full specs.

#### Free-Tier Models (7 models)

Models with the `-free` suffix are offered at zero cost for a limited
promotional period through OpenCode Zen. Data submitted to these models may
be collected and used for model improvement.

| Model ID | Base Model | Context | Output | Notes |
|----------|-----------|---------|--------|-------|
| `big-pickle` | Unknown (stealth) | — | — | OpenCode-exclusive "stealth model". Free for limited time. No public docs. |
| `deepseek-v4-flash-free` | deepseek-v4-flash | 1M | 384K | Free tier of DeepSeek V4 Flash. |
| `mimo-v2.5-free` | mimo-v2.5 | 1M | 131K | Free tier of MiMo V2.5. |
| `qwen3.6-plus-free` | qwen3.6-plus | 1M | 65K | Free tier of Qwen3.6 Plus. |
| `minimax-m3-free` | minimax-m3 | 1M* | —* | Free tier of MiniMax M3. |
| `nemotron-3-ultra-free` | NVIDIA (unreleased) | — | — | Not on NVIDIA's public platform. Likely Zen-exclusive or unreleased variant. |
| `nemotron-3-super-free` | NVIDIA Nemotron-3-Super-120B | 1M | 16K | Hybrid Mamba-Transformer MoE. Thinking supported via `enable_thinking`. |

---

## 5. models.dev — The Upstream Metadata Catalog

### 5.1 Overview

```text
URL:     https://models.dev/models.json
Method:  GET
Auth:    None
Format:  JSON (flat object)
```

This is the canonical source for model metadata. OpenCode itself references
this catalog — the OpenCode config schema at `https://opencode.ai/config.json`
points to `https://models.dev/model-schema.json` for model ID validation.

### 5.2 Response Shape

The response is a flat object keyed by `"provider/model-id"`:

```json
{
  "deepseek/deepseek-v4-pro": {
    "id": "deepseek/deepseek-v4-pro",
    "name": "DeepSeek V4 Pro",
    "family": "deepseek-thinking",
    "attachment": false,
    "reasoning": true,
    "tool_call": true,
    "temperature": true,
    "knowledge": "2025-05",
    "release_date": "2026-04-24",
    "last_updated": "2026-06-01",
    "modalities": { "input": ["text"], "output": ["text"] },
    "open_weights": true,
    "limit": { "context": 1000000, "output": 384000 },
    "weights": [{ "label": "Hugging Face", "url": "..." }],
    "benchmarks": [{ "name": "...", "score": ... }]
  },
  "anthropic/claude-sonnet-4-6": { ... },
  "openai/gpt-5.5": { ... }
}
```

### 5.3 Field Inventory

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Fully qualified ID in `provider/model` format |
| `name` | string | Human-readable display name |
| `family` | string | Model family grouping (e.g. `gpt`, `claude-opus`, `deepseek-thinking`) |
| `attachment` | boolean | Supports file attachments |
| `reasoning` | boolean | Supports thinking/reasoning mode |
| `tool_call` | boolean | Supports function/tool calling |
| `temperature` | boolean | Supports temperature parameter |
| `knowledge` | string | Training data cutoff (e.g. `"2025-05"`) |
| `release_date` | string | Release date (e.g. `"2026-04-24"`) |
| `last_updated` | string | Last metadata update |
| `modalities.input` | string[] | Input types: `text`, `image`, `video`, `audio`, `pdf` |
| `modalities.output` | string[] | Output types: `text` |
| `open_weights` | boolean | Whether model has open-weight release |
| `limit.context` | number | Maximum context window (tokens) |
| `limit.input` | number | Maximum input tokens (some models only) |
| `limit.output` | number | Maximum output tokens |
| `interleaved` | boolean or object | Interleaved thinking. If object: `{ "field": "reasoning_content" }` |
| `cost.input` | number | Cost per input token |
| `cost.output` | number | Cost per output token |
| `cost.cache_read` | number | Cost per cached read token |
| `cost.cache_write` | number | Cost per cache write token |
| `weights` | array | Download links for open-weight models |
| `benchmarks` | array | Benchmark scores |

### 5.4 Coverage vs OpenCode Models

37 of 51 unique OpenCode model IDs were found in models.dev.

**Not found (14 models):**

```text
big-pickle                          # OpenCode-exclusive stealth model
claude-sonnet-4                     # Alias — claude-sonnet-4-20250514 exists
deepseek-v4-flash-free              # Zen free-tier variant
gemini-3-flash                      # Deprecated — gemini-3-flash-preview exists
gemini-3.1-pro                      # Variant name — gemini-3.1-pro-preview exists
gpt-5.3-codex-spark                 # Not in models.dev
mimo-v2.5-free                      # Zen free-tier variant
minimax-m2.5                        # Not in models.dev (MiniMax not catalogued)
minimax-m2.7                        # Not in models.dev
minimax-m3                          # Not in models.dev
minimax-m3-free                     # Zen free-tier variant
nemotron-3-super-free               # Zen free-tier variant
nemotron-3-ultra-free               # Zen free-tier variant
qwen3.6-plus-free                   # Zen free-tier variant
```

### 5.5 Fetching Metadata for a Specific Model

```bash
# Get all metadata for DeepSeek V4 Pro
curl -s https://models.dev/models.json | \
  python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)['deepseek/deepseek-v4-pro'], indent=2))"

# Get just the context/output limits
curl -s https://models.dev/models.json | \
  python3 -c "import sys,json; d=json.load(sys.stdin); print(d['anthropic/claude-opus-4-8']['limit'])"
```

### 5.6 Building a Local Catalog

To build a local JSON catalog filtered to OpenCode Go models:

```bash
GO_MODELS=$(curl -s https://opencode.ai/zen/go/v1/models | python3 -c "
import sys, json
ids = [m['id'] for m in json.load(sys.stdin)['data']]
print(json.dumps(ids))
")

curl -s https://models.dev/models.json | python3 -c "
import sys, json
models_dev = json.load(sys.stdin)
go_ids = $GO_MODELS
catalog = {}
for key, val in models_dev.items():
    if val['id'].split('/', 1)[-1] in go_ids:
        catalog[key] = val
print(json.dumps(catalog, indent=2))
" > opencode-go-catalog.json
```

---

## 6. models.dev Provider Schema

### 6.1 Overview

```text
URL:     https://models.dev/api.json
Method:  GET
Auth:    None
Format:  JSON (object keyed by provider ID)
```

This endpoint returns provider-level configuration, including the NPM package
to use, API base URL, environment variables, and all models with full metadata.

### 6.2 Response Shape

```json
{
  "deepseek": {
    "id": "deepseek",
    "name": "DeepSeek",
    "npm": "@ai-sdk/deepseek",
    "api": "https://api.deepseek.com/v1",
    "env": ["DEEPSEEK_API_KEY"],
    "doc": "https://api-docs.deepseek.com/",
    "models": {
      "deepseek-v4-pro": {
        "id": "deepseek-v4-pro",
        "name": "DeepSeek V4 Pro",
        "family": "deepseek-thinking",
        "limit": { "context": 1000000, "output": 384000 },
        "reasoning": true,
        "tool_call": true,
        ...
      }
    }
  }
}
```

### 6.3 Provider Schema Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Provider identifier |
| `name` | string | Display name |
| `npm` | string | AI SDK NPM package (e.g. `@ai-sdk/openai-compatible`) |
| `api` | string | Default API base URL |
| `env` | string[] | Environment variable names for API keys |
| `doc` | string | Documentation URL |
| `models` | object | All models with full metadata (same fields as Section 5.3) |

### 6.4 Relevant Provider IDs

| Provider ID | Name | NPM Package |
|-------------|------|-------------|
| `anthropic` | Anthropic | `@ai-sdk/anthropic` |
| `openai` | OpenAI | `@ai-sdk/openai` |
| `google` | Google | `@ai-sdk/google` |
| `deepseek` | DeepSeek | `@ai-sdk/deepseek` |
| `zhipuai` | Zhipu (GLM) | `@ai-sdk/openai-compatible` |
| `moonshotai` | Moonshot (Kimi) | `@ai-sdk/openai-compatible` |
| `xiaomi` | Xiaomi (MiMo) | `@ai-sdk/openai-compatible` |
| `alibaba` | Alibaba (Qwen) | `@ai-sdk/openai-compatible` |
| `minimaxi` | MiniMax | `@ai-sdk/openai-compatible` |
| `tencent` | Tencent (Hy) | `@ai-sdk/openai-compatible` |
| `xai` | xAI | `@ai-sdk/openai-compatible` |
| `nvidia` | NVIDIA | `@ai-sdk/openai-compatible` |

---

## 7. OpenCode Config Schema

### 7.1 Overview

```text
URL:     https://opencode.ai/config.json
Method:  GET
Auth:    None
Format:  JSON Schema ($schema: http://json-schema.org/draft-07/schema#)
```

The OpenCode config schema defines per-model override capabilities that map
directly to the metadata fields in models.dev. This is useful for
understanding what metadata fields matter for configuration.

### 7.2 Per-Model Override Fields

Defined in `provider.<id>.models.<model-key>`:

**Limits:**

```text
limit.context   — number (required) — Maximum context window (tokens)
limit.input     — number (optional) — Maximum input size (tokens)
limit.output    — number (required) — Maximum output size (tokens)
```

**Capability flags:**

```text
attachment      — boolean — Supports file attachments
reasoning       — boolean — Supports thinking/reasoning mode
temperature     — boolean — Supports temperature parameter
tool_call       — boolean — Supports function/tool calling
interleaved     — boolean or object — Interleaved thinking support
                              If object: { "field": "reasoning_content" | "reasoning_details" }
```

**Modalities:**

```text
modalities.input  — string[] — "text", "audio", "image", "video", "pdf"
modalities.output — string[] — Same enum as input
```

**Cost:**

```text
cost.input          — number — Cost per input token
cost.output         — number — Cost per output token
cost.cache_read     — number — Cost per cached read token
cost.cache_write    — number — Cost per cache write token
cost.context_over_200k — object — Separate pricing above 200K context
```

**Identity:**

```text
id              — string — Model identifier
name            — string — Display name
family          — string — Model family grouping
release_date    — string — Release date
experimental    — boolean — Marks experimental models
status          — enum — "alpha", "beta", "deprecated", "active"
```

### 7.3 Sample Per-Model Config

```json
{
  "provider": {
    "anthropic": {
      "models": {
        "claude-opus-4-6": {
          "limit": { "context": 200000, "output": 32000 },
          "attachment": true,
          "reasoning": true,
          "temperature": true,
          "tool_call": true,
          "cost": {
            "input": 0.000015,
            "output": 0.000075,
            "cache_read": 0.0000015,
            "cache_write": 0.00001875
          },
          "modalities": {
            "input": ["text", "image", "pdf"],
            "output": ["text"]
          }
        }
      }
    }
  }
}
```

---

## 8. Per-Provider API Format Reference

| Provider | Native API | API Path Pattern | Auth Style | OpenAI-Compatible | Thinking Support |
|----------|-----------|-----------------|------------|-------------------|-----------------|
| Anthropic | Messages API | `/v1/messages` | `x-api-key` header | No (via proxy only) | Extended + adaptive thinking |
| OpenAI | Chat Completions + Responses | `/v1/chat/completions`, `/v1/responses` | `Authorization: Bearer` | Native | Reasoning mode (all GPT-5+) |
| Google | Gemini Native | `/v1/models/{model}:generateContent` | `x-goog-api-key` or OAuth | Partial (via proxy) | `thinkingLevel` parameter |
| DeepSeek | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | Enabled by default, also Anthropic-compatible |
| GLM/Zhipu | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | `enable_thinking` parameter |
| Kimi/Moonshot | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | `thinking` parameter, default enabled |
| Qwen/Alibaba | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | `enable_thinking` parameter, 81920 thinking tokens |
| MiniMax | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | Not documented publicly |
| MiMo/Xiaomi | OpenAI-compatible (SGLang/vLLM) | `/v1/chat/completions` | `Authorization: Bearer` | Yes | `enable_thinking` parameter |
| Tencent | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | `enable_thinking` parameter |
| xAI | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | Reasoning mode |
| NVIDIA | OpenAI-compatible | `/v1/chat/completions` | `Authorization: Bearer` | Yes | `enable_thinking` + `reasoning_budget` |

### 8.1 Key Observations

- All providers accessible through OpenCode are OpenAI-compatible for chat
  completions. Anthropic and Google have their own native APIs that OpenCode
  supports via separate adapters.
- DeepSeek uniquely supports both OpenAI and Anthropic-compatible endpoints
  natively (base URLs differ).
- Thinking/reasoning is enabled by default on DeepSeek V4, Kimi K2.6.
- Qwen Plus uses `enable_thinking` with up to 81,920 thinking tokens.
- Gemini uses `thinkingLevel` (not `thinkingBudget` which is for Gemini 2.5).

---

## 9. Provider TOML Config Templates

### 9.1 opencode-go

```toml
[provider]
name = "opencode-go"
api_key = "${LLM_PROXY_OPENCODE_GO_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://opencode.ai/zen/go/v1/chat/completions"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/go/v1/messages"

[provider.routes]
chat_completions = "chat"
messages = "anthropic"

# [provider.model_aliases]
# "alias" = "upstream-model-id"

# [provider.discovery]
# kind = "openai_compatible_models"
# endpoint = "https://opencode.ai/zen/go/v1/models"
```

**Adapter mapping:**

| Route Kind | Adapter | Protocol | Upstream |
|------------|---------|----------|----------|
| `chat_completions` | `chat` | OpenAI Chat Completions | `/zen/go/v1/chat/completions` |
| `messages` | `anthropic` | Anthropic Messages | `/zen/go/v1/messages` |

### 9.2 opencode-zen

```toml
[provider]
name = "opencode-zen"
api_key = "${LLM_PROXY_OPENCODE_ZEN_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://opencode.ai/zen/v1/chat/completions"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/v1/messages"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://opencode.ai/zen/v1/responses"

[provider.adapters.gemini]
protocol = "gemini_generate_content"
endpoint = "https://opencode.ai/zen/v1/models/{model}:generateContent"

[provider.routes]
chat_completions = "chat"
messages = "anthropic"

# [provider.model_aliases]
# "alias" = "upstream-model-id"

# [provider.discovery]
# kind = "openai_compatible_models"
# endpoint = "https://opencode.ai/zen/v1/models"
```

**Adapter mapping:**

| Route Kind | Adapter | Protocol | Upstream |
|------------|---------|----------|----------|
| `chat_completions` | `chat` | OpenAI Chat Completions | `/zen/v1/chat/completions` |
| `messages` | `anthropic` | Anthropic Messages | `/zen/v1/messages` |
| (no route) | `responses` | OpenAI Responses | `/zen/v1/responses` |
| (no route) | `gemini` | Gemini GenerateContent | `/zen/v1/models/{model}:generateContent` |

Notes:

- Zen has 4 adapters but only 2 route kinds. The `responses` and `gemini`
  adapters are available for direct use but not mapped to default route kinds.
- The Gemini adapter uses `{model}` as a URL template parameter that gets
  substituted at request time.
- Both templates use `auth_style = "bearer"` since OpenCode expects
  `Authorization: Bearer <key>` for all endpoints.

---

## 10. Free-Tier Model Notes

### 10.1 What "-free" Means

In the OpenCode Zen context, the `-free` suffix designates models offered at
zero cost for a **limited promotional period**. These models are accessible
through the standard OpenCode Zen gateway at no charge while the respective
teams collect user feedback to improve the models.

### 10.2 Privacy Implications

Data submitted to free-tier models may be collected and used for model
improvement during the free period. NVIDIA free endpoints specifically note:
"Do not submit personal or confidential data. Usage is logged for security
and to improve NVIDIA products and services."

### 10.3 Known Free Models and Their Base Models

| Free Model ID | Base Model | Provider |
|---------------|-----------|----------|
| `big-pickle` | Unknown | OpenCode (stealth) |
| `deepseek-v4-flash-free` | `deepseek-v4-flash` | DeepSeek |
| `mimo-v2.5-free` | `mimo-v2.5` | Xiaomi |
| `qwen3.6-plus-free` | `qwen3.6-plus` | Alibaba |
| `minimax-m3-free` | `minimax-m3` | MiniMax |
| `nemotron-3-ultra-free` | `nemotron-3-ultra` (unreleased) | NVIDIA |
| `nemotron-3-super-free` | `nemotron-3-super-120b-a12b` | NVIDIA |

### 10.4 Stability Warning

Free models may be removed or transitioned to paid at any time. Do not rely
on them for production configurations. They are useful for testing and
evaluation only.

---

## 11. Quick Reference: Script Recipes

### 11.1 Fetch Go Plan Model IDs

```bash
curl -s https://opencode.ai/zen/go/v1/models | \
  python3 -c "import sys,json; [print(m['id']) for m in json.load(sys.stdin)['data']]"
```

### 11.2 Fetch Zen Plan Model IDs

```bash
curl -s https://opencode.ai/zen/v1/models | \
  python3 -c "import sys,json; [print(m['id']) for m in json.load(sys.stdin)['data']]"
```

### 11.3 Get Full Metadata for a Single Model

```bash
curl -s https://models.dev/models.json | \
  python3 -c "
import sys, json
d = json.load(sys.stdin)
for key, val in d.items():
    if val.get('id','').endswith('/claude-opus-4-8'):
        print(json.dumps(val, indent=2))
"
```

### 11.4 Build a Combined Catalog (OpenCode IDs + models.dev Metadata)

```bash
#!/usr/bin/env python3
"""Build a local JSON catalog merging OpenCode model lists with models.dev metadata."""
import json, urllib.request

# Fetch OpenCode model lists
go_resp = urllib.request.urlopen("https://opencode.ai/zen/go/v1/models").read()
zen_resp = urllib.request.urlopen("https://opencode.ai/zen/v1/models").read()
go_ids = {m["id"] for m in json.loads(go_resp)["data"]}
zen_ids = {m["id"] for m in json.loads(zen_resp)["data"]}

# Fetch models.dev metadata
dev_resp = urllib.request.urlopen("https://models.dev/models.json").read()
dev = json.loads(dev_resp)

# Build catalog
catalog = {}
for key, val in dev.items():
    model_id = val["id"].split("/", 1)[-1]
    if model_id in go_ids or model_id in zen_ids:
        catalog[key] = {
            **val,
            "opencode_go": model_id in go_ids,
            "opencode_zen": model_id in zen_ids,
        }

# Add entries for models not found in models.dev
all_opencode = go_ids | zen_ids
found = {val["id"].split("/", 1)[-1] for val in dev.values() if val["id"].split("/", 1)[-1] in all_opencode}
missing = sorted(all_opencode - found)
for mid in missing:
    catalog[f"opencode/{mid}"] = {
        "id": f"opencode/{mid}",
        "name": mid,
        "opencode_go": mid in go_ids,
        "opencode_zen": mid in zen_ids,
        "limit": {},
        "reasoning": None,
        "tool_call": None,
        "temperature": None,
        "attachment": None,
        "modalities": {"input": [], "output": []},
    }

print(json.dumps(catalog, indent=2))
```

---

## Research Sources

- OpenCode Go model listing: https://opencode.ai/zen/go/v1/models
- OpenCode Zen model listing: https://opencode.ai/zen/v1/models
- models.dev model metadata catalog: https://models.dev/models.json
- models.dev provider schema: https://models.dev/api.json
- models.dev model ID schema: https://models.dev/model-schema.json
- OpenCode config schema: https://opencode.ai/config.json
- OpenCode providers documentation: https://opencode.ai/docs/providers
- Anthropic model specs: https://platform.claude.com/docs/en/about-claude/models
- Google Gemini model specs: https://ai.google.dev/gemini-api/docs/models
- DeepSeek API docs: https://api-docs.deepseek.com/
- Qwen/Alibaba model docs: https://help.aliyun.com/zh/model-studio/getting-started/models
- GLM/Zhipu model docs: https://open.bigmodel.cn/dev/howuse/model
- Kimi/Moonshot API docs: https://platform.moonshot.cn/docs/api/chat
- Kimi/Moonshot pricing: https://platform.moonshot.cn/docs/pricing/chat
- MiniMax M3 docs: https://www.minimaxi.com/document/guides/chat/model?id=M3
- MiMo V2.5 Pro weights: https://huggingface.co/XiaomiMiMo/MiMo-V2.5-Pro
- MiMo V2.5 weights: https://huggingface.co/XiaomiMiMo/MiMo-V2.5
- MiMo V2 Flash weights: https://huggingface.co/XiaomiMiMo/MiMo-V2-Flash
- MiniMax M2.5 weights: https://huggingface.co/MiniMaxAI/MiniMax-M2.5
- MiniMax M2.7 weights: https://huggingface.co/MiniMaxAI/MiniMax-M2.7
- xAI model docs: https://docs.x.ai/docs/models
- NVIDIA NIM platform: https://build.nvidia.com
