# Provider Compatibility

Provider support is more than choosing a base URL. Authentication, endpoint
construction, request capabilities, response repair, and streaming contracts
can all vary behind otherwise familiar APIs.

These are implementation notes for current and possible future adapters, not a
support matrix.

## Anthropic and Claude routes

- Strip empty text blocks and invalid replayed thinking blocks; native Messages
  rejects histories that some OpenAI-compatible backends tolerate.
- Sanitize tool names and schemas, preserve the reverse name map, and keep
  `tool_use`/`tool_result` ordering valid.
- Consume Anthropic-only fields after translating them. Raw `output_config`,
  beta features, context-management fields, or cache controls must not leak
  into a non-Anthropic backend.
- Legacy thinking, adaptive thinking, effort, and signed reasoning require
  model-aware handling rather than a direct field copy.
- OAuth tokens use bearer auth and provider beta headers, not the normal
  `x-api-key` path.
- Anthropic-to-Responses is a schema bridge: thinking becomes reasoning,
  structured output becomes `text.format`, context edits become compaction,
  and user identity may need target-specific limits.
- SAP, Vertex, Azure, and Bedrock Claude surfaces add their own response repair,
  auth, header, version, and parameter restrictions.

## AWS, Bedrock, and SageMaker

Native AWS integration requires SigV4 plus region, service, ARN, role, and
credential-refresh handling. Static keys, ambient credentials, AssumeRole,
web identity, and profiles must not share unsafe cache lifetimes.

Bedrock has multiple protocol families:

- Invoke and Converse rewrite OpenAI/Anthropic request shapes differently.
- Structured output may be prompt-inlined instead of sent as a native field.
- Guardrails may require `guarded_text` content blocks.
- Model IDs need normalization across ARNs, cross-region prefixes, throughput
  suffixes, and routing prefixes.
- Converse request metadata and grounding tools have provider limits.
- Invoke Agent is a session/trace protocol, not a chat-completion path.

SageMaker endpoints may use Hugging Face prompt templates and `inputs` plus
`parameters` rather than chat messages. Model-family transforms should be
explicit, including temperature/token floors and response-envelope variants.

## Azure

An exact Azure OpenAI-compatible URL may work through a generic adapter, but
native Azure support needs more:

- Preserve an `api-version` already embedded in the base URL and normalize v1
  path variants without duplicating `/openai`.
- Support Azure API keys and Azure AD bearer tokens with clear precedence.
- Apply model/API-version capability rules, such as dropping unsupported
  Responses `temperature` or repairing reasoning items.
- Keep Azure Anthropic headers and body translation separate from Azure OpenAI
  behavior.
- Treat Assistants and AI Agents as thread/run workflows with polling,
  citations, and conversation identity—not single chat requests.
- Normalize provider-specific lifecycle statuses and null fields before
  exposing OpenAI-shaped objects.

## Gemini and Vertex

- Select `response_schema` versus `response_json_schema` by model capability
  and preserve property ordering where Vertex requires it.
- Normalize `generationConfig`/`config` at the correct API boundary.
- System instructions may require synthetic user/model turns when the target
  surface lacks an equivalent slot.
- Normalize MIME aliases and decide between `file_data`, `inline_data`, and
  uploaded files before request encoding.
- Standalone usage frames must be buffered until they can be associated with a
  response.
- Translate `citationSources` into a stable citations representation.
- Never send process-wide Google credentials to a caller-supplied custom host;
  require an explicit key for custom `api_base` values.
- Vertex partner models can require publisher-specific URLs, bearer auth,
  version fields, and removal of the model from the request body.

## OpenAI, Responses, and compatible gateways

OpenAI-compatible providers frequently need small but important repairs:

- Rewrite `max_completion_tokens` to `max_tokens` only when the target expects
  the older field.
- Normalize null roles, token counts, logprob lists, and error codes before
  typed validation.
- Preserve the distinction among Responses `completed`, `failed`, and
  `incomplete` events.
- Repair missing output/content indices and accumulate function-call arguments
  by stable index.
- Use explicit parameter allowlists for model families that expose only a
  subset of OpenAI behavior.
- Keep gateway routing knobs under an opaque `extra_body` when the OpenAI
  client surface cannot represent them.
- Preserve provider cost, retries, citations, and routing identity as bounded
  metadata rather than inventing standard fields.

Examples include:

- OpenRouter: route/model/transforms live in `extra_body`; usage cost and
  reasoning may require opt-in fields and response metadata.
- Vercel AI Gateway: provider options are nested under `extra_body`.
- Perplexity: HTTP 200 can still contain a failed Responses object, and search
  citations are side-channel data.
- Volcengine: Responses events may need missing response fields filled before
  validation.
- Databricks: strip provider prefixes and keep Responses on HTTP.
- GitHub Copilot/ChatGPT subscription APIs: synthesize provider headers and
  session/account identity from authenticated metadata; never treat them as a
  plain API-key endpoint.

## Local and self-hosted OpenAI-compatible runtimes

- Ollama uses `/api/chat`, provider-prefix normalization, local parameter names,
  and sometimes `<think>` stream boundaries.
- Hosted vLLM requires caller-provided URLs for several surfaces and may need a
  harmless placeholder credential because client libraries require one.
- LM Studio may also need placeholder auth and a structured-output schema
  rewrite.
- Hugging Face and TGI endpoints can select different request envelopes from
  model task metadata and may reconstruct streaming output.
- Heroku and similar gateways may require path completion and flattening of
  list-shaped message content.

Placeholder credentials must never be confused with real authentication or
forwarded beyond the configured local endpoint.

## Other provider transformation patterns

Keep provider-specific behavior at the edge:

- Mistral: sanitize schemas and collapse content-list responses into text plus
  reasoning.
- DeepInfra: flatten tool messages, constrain tool choice, and unwrap nested
  error details.
- Nvidia NIM: use model-specific parameter allowlists and endpoint-specific
  request shapes.
- Cohere: handle tool-result history and distinguish text from image embedding
  inputs.
- IBM WatsonX: split tool-choice forms, use deployment-specific URLs, and apply
  model-family prompt templates only where necessary.
- GigaChat and Ollama: structured output may be synthesized through tool calls
  or JSON response parsing.
- Baseten and other deployment services: model identity may select a dedicated
  host rather than a shared API path.

Do not add a one-off transform without a fixture that proves the exact request
and response behavior it protects.

## OpenCode Go and Zen

OpenCode exposes OpenAI-compatible gateways used by the example provider
files:

```text
Go:  https://opencode.ai/zen/go/v1
Zen: https://opencode.ai/zen/v1
```

Stable surfaces used by this project:

```text
GET  {base}/models
POST {base}/chat/completions
POST {base}/messages
POST {zen-base}/responses
POST {zen-base}/models/{model}:generateContent
```

Use `providers/opencode-go.toml.example` and
`providers/opencode-zen.toml.example` as the configuration source. Model lists,
pricing, context limits, and free-tier status are intentionally omitted here:
they are volatile and should come from live discovery or a maintained catalog.

The `/models` response is OpenAI-shaped but contains limited metadata. If richer
capabilities are imported from an external catalog, join by exact model ID,
record the catalog source and retrieval time, and allow local overrides. Never
infer routing support solely from catalog metadata.

## Provider adapter checklist

- Authentication type and secret ownership.
- Base URL normalization and version/query precedence.
- Request-field allowlist and semantic-drop policy.
- Tool, structured-output, reasoning, and multimodal capabilities.
- Non-streaming response repair and metadata preservation.
- Streaming event ordering, terminator, usage, and error behavior.
- Provider-reported versus locally estimated usage and cost.
- Cancellation, retry safety, and credential cooldown behavior.
- Golden fixtures for every provider-specific transform.
