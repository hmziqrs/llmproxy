# Mini Quirks — Usage, Cost & Quota

## Never invent usage

Some references synthesize final usage chunks so clients/loggers see a
clean finish. That is operationally useful but dangerous if synthetic
usage is later treated as provider truth.

Design rule:

```text
Synthetic usage must be marked synthetic.
```

Core usage should carry provenance:

```text
provider_reported
estimated
synthetic_zero
unknown
```

## Usage token nulls are zero-filled before validation

Some OpenAI-like providers return `usage` objects where one or more
`*_tokens` fields are `null`. LiteLLM rewrites those null counts to `0` before
the typed response model runs, so the usage payload still validates.

Design rule:

```text
Usage shape repair belongs at the adapter boundary.
```

Track:

```text
usage_null_token_zero_fill
usage_shape_repair
```

## Credential and quota routing

Subscription, OAuth, and rotated-key providers behave like quota systems,
not just static API keys. The references include sticky credential sessions,
blocked-credential waits, cooldowns, learned quota estimates, and
provider-specific cost skips.

Design rule:

```text
Credential selection is routing state.
Usage accounting is not the same thing as quota estimation.
```

Track separately:

```text
provider_reported_usage
estimated_cost
quota_bucket
credential_affinity
cooldown_until
quota_confidence
```

Useful policy knobs:

- Sticky credential for conversation/container/session routes.
- Bounded wait when the preferred credential is temporarily blocked.
- Cooldown when a provider reports rate-limit or quota exhaustion.
- Explicit `unknown` or `skip_cost` mode for providers with unreliable cost data.

## Usage metadata sometimes must be requested

Several references inject usage flags:

- OpenAI-style streams get `stream_options.include_usage = true`.
- OpenRouter requests get `usage.include = true` so cost data comes back.
- Some providers still have unreliable or skipped cost calculation.

Design rule:

```text
Usage collection is a request feature with provider-specific opt-in fields.
```

Track:

```text
usage_requested = true | false
usage_request_field = stream_options.include_usage | usage.include | provider_specific
usage_response_source = body | headers | hidden_params | synthetic | unknown
```

Do not assume usage absence means zero tokens or zero cost.

## Token-count endpoints are provider APIs, not local estimates

Anthropic token counting requires a beta header and accepts messages,
system, and tools for accurate counting. Gemini countTokens needs Gemini
content format and strips unsupported fields like function response IDs
before sending.

Design rule:

```text
Token counting has provenance.
```

Track:

```text
provider_counted
local_estimate
translated_then_counted
count_includes_tools
count_includes_system
count_sanitized_fields
```

A token count used for routing/fallback should say whether it came from the
provider, a local tokenizer, or a translated approximation.

## Cost estimate endpoints are estimates with source labels

One proxy exposes `/v1/cost-estimate`, first using its model-info service
and then falling back to LiteLLM pricing data. If both miss, it returns
unknown rather than inventing a price.

Design rule:

```text
Cost estimates need source and confidence.
```

Track:

```text
cost_value
currency
pricing_source
pricing_version
includes_cache_read
includes_cache_creation
confidence = provider_reported | catalog_estimate | fallback_estimate | unknown
```

Cost estimation should not be mixed up with actual billed usage.

## Passthrough cost is precomputed and injected into hidden params

For image generation and image editing passthroughs, LiteLLM creates a small
synthetic `ImageResponse`, computes the cost itself, and stores
`response_cost` in `_hidden_params`. That prevents downstream logging from
recalculating the same charge through the generic completion path.

Design rule:

```text
If the proxy already computed passthrough cost, stash it where downstream loggers will trust it.
```

Track:

```text
image_passthrough_cost
response_cost_hidden_param
synthetic_image_response
recalc_prevention
```

## Gemini countTokens strips unsupported functionResponse IDs

The Gemini count-tokens helper deep-copies the request contents and removes the
`id` field from every `functionResponse` block before calling the API. The
provider rejects that field, so token counting needs a special cleanup pass
even though the runtime generation path may carry richer tool metadata.

Design rule:

```text
Counting endpoints often have stricter schemas than generation endpoints.
```

Track:

```text
gemini_count_tokens_strip_function_response_id
gemini_count_tokens_content_cleanup
gemini_count_tokens_schema_split
```

## OpenAI Responses token counting is its own schema bridge

The Responses token-count helper does not forward chat messages as-is. It
rewrites chat history into a Responses-style `input` array, lifts
`system`/`developer` messages into `instructions`, and maps chat `tool_calls`
into `function_call` items while converting `tool` messages into
`function_call_output` items. It also rewrites tool definitions from the chat
shape into the Responses shape.

Design rule:

```text
Counting endpoints often need a protocol-specific request bridge, not a raw copy of the generation payload.
```

Track:

```text
openai_responses_count_tokens_input_bridge
openai_responses_count_tokens_instructions_bridge
openai_responses_count_tokens_tool_rewrite
openai_responses_count_tokens_function_call_output
```

## Vertex partner-model token counting strips version suffixes

Vertex AI partner-model token counting does not accept model names with
version suffixes such as `@default` or `@20251001`. The counter strips the
suffix from both the requested model name and any `request_data["model"]`
value before it builds the count-tokens endpoint and sends the request.

Design rule:

```text
Token-count endpoints may require a versionless model alias even when the main
generation route accepts a versioned one.
```

Track:

```text
vertex_partner_count_tokens_strip_version_suffix
vertex_partner_count_tokens_model_rewrite
vertex_partner_count_tokens_versionless_alias
```

## xAI chat repairs tool-call finish reasons and folds usage back into OpenAI shape

The xAI chat adapter does not leave upstream usage and finish reasons alone.
If the model returns an empty `finish_reason` while tool calls are present, the
adapter rewrites it to `tool_calls`. It also folds `reasoning_tokens` into
`completion_tokens` so total usage matches the OpenAI invariant, and maps
`num_sources_used` into `prompt_tokens_details.web_search_requests` for web
search accounting. On streaming responses, xAI can emit a final usage chunk
with an empty `choices` array, so the iterator injects a dummy choice before
normal parsing.

Design rule:

```text
Provider-specific usage and terminal-state quirks may need to be reconciled
before the shared response model can stay internally consistent.
```

Track:

```text
xai_empty_finish_reason_to_tool_calls
xai_reasoning_tokens_fold_into_completion_tokens
xai_web_search_sources_to_prompt_details
xai_stream_usage_chunk_dummy_choice
```

## OpenRouter chat exposes reasoning only for supported models and lifts cost into hidden headers

OpenRouter chat does not expose the full reasoning surface to every model. It
adds `reasoning_effort` and `thinking` only when the model is flagged as
reasoning-capable, rewrites streamed `delta.reasoning` into
`delta.reasoning_content`, and copies `usage.cost` into
`_hidden_params["additional_headers"]["llm_provider-x-litellm-response-cost"]`
when cost data is present.

Design rule:

```text
Reasoning-capable model gating and response-cost extraction are protocol
behavior, not cosmetic metadata.
```

Track:

```text
openrouter_reasoning_param_gating
openrouter_stream_reasoning_content
openrouter_usage_cost_hidden_header
```

## Azure AI Foundry Model Router strips routing prefixes before dispatch and adds a flat infrastructure cost

Azure AI Foundry Model Router is a real routing bridge, not a plain Azure chat
deployment. The adapter strips the `model_router/` prefix before sending the
request to Azure so the deployment name reaches the API without the routing
wrapper. On the response side it preserves the actual model returned by Azure
for display and cost tracking, then applies an extra flat infrastructure charge
based on prompt tokens via the Azure model-router cost calculator.

Design rule:

```text
Routing prefixes can be request-only metadata; the response model and cost model may need to be reconstructed separately.
```

Track:

```text
azure_model_router_prefix_strip
azure_model_router_actual_model_preservation
azure_model_router_flat_infra_cost
azure_model_router_request_vs_billed_model_split
```
