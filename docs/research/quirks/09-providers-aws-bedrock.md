# Mini Quirks — Provider Quirks: AWS / Bedrock

## Bedrock Invoke inlines structured output into the last user message

Bedrock Invoke does not forward Anthropic `output_config.format` directly.
The adapter removes the nested `format` key, keeps any remaining
`output_config` fields like `effort`, and when the schema is present it
embeds the structured-output prompt into the final user message instead of
relying on a native response-format field.

Design rule:

```text
Bedrock Invoke structured output is prompt-inlined, not field-forwarded.
```

Track:

```text
bedrock_invoke_output_config_format_pop
bedrock_invoke_inline_structured_schema
bedrock_invoke_keep_output_config_effort
```

## Bedrock Converse rewrites guarded text for guardrails

When `guardrailConfig` is present, Bedrock Converse rewrites consecutive
trailing user messages into `guarded_text` blocks. Plain string content is
wrapped as a single `guarded_text` item, and existing `text` blocks are
converted one by one unless the message already contains `guarded_text`.

Design rule:

```text
Guardrail-enabled message content needs a pre-dispatch content-type rewrite.
```

Track:

```text
bedrock_converse_guarded_text_rewrite
bedrock_guardrail_content_conversion
bedrock_trailing_user_guardrails
```

## Bedrock model names are stripped before base-model resolution

Bedrock model identity is not the raw string the caller passed in. The helper
strips LiteLLM routing prefixes, extracts the trailing model from ARNs, removes
throughput/context-window suffixes, and then handles cross-region inference
prefixes like `us.` or region path prefixes before returning the base model.

Design rule:

```text
Bedrock model identity must be normalized before any routing or capability lookup.
```

Track:

```text
bedrock_strip_routing_prefix
bedrock_strip_throughput_suffix
bedrock_cross_region_prefix_handling
bedrock_arn_model_extraction
```

## Sagemaker completion turns OpenAI prompts into Hugging Face inference payloads

The Sagemaker adapter does not send messages through as chat JSON. It turns
the conversation into a single prompt string using either a custom prompt
template, a model-specific Hugging Face template override, or a fallback
Llama template chosen from the model name. The request body is then sent as
`inputs` plus a `parameters` block.

It also has provider-specific guardrails: `temperature=0` is bumped to `0.01`
unless the caller sets `aws_sagemaker_allow_zero_temp`, and zero token limits
are floored to `1` so the HF backend does not reject the request.

Design rule:

```text
Hugging Face-backed endpoints need prompt synthesis and safety floors before dispatch.
```

Track:

```text
sagemaker_prompt_template_selection
sagemaker_llama2_template_fallback
sagemaker_zero_temperature_floor
sagemaker_zero_max_tokens_floor
sagemaker_allow_zero_temp_escape_hatch
```

## Cloudflare AI Run returns a provider-native result payload, not an OpenAI envelope

The Cloudflare chat adapter does not receive a standard OpenAI `choices`
response. It reads `completion_response["result"]` and then chooses between
`result.response` and `result.response_text` depending on which key the model
produced. The streaming iterator follows the same text-first rule and only
emits those response fields.

Design rule:

```text
Some providers return a top-level result object that must be flattened before the core response model sees it.
```

Track:

```text
cloudflare_result_response_alias
cloudflare_result_response_text_alias
cloudflare_stream_text_only
```

## Bedrock Converse and Invoke both rewrite request shape before dispatch

Bedrock does not accept the raw OpenAI shape unchanged. Converse validates
`requestMetadata` against Bedrock’s key count and character limits, turns
`web_search_options` into `systemTool={"name":"nova_grounding"}`, and treats
an empty `web_search_options` dict as an explicit grounding enablement. The
Invoke path also appends the output schema to the last user message instead of
sending `output_format` directly, and Bedrock grounding citations are
reassembled into OpenAI-style annotations plus content text.

Design rule:

```text
When a provider has multiple invocation surfaces, each one can carry its own
request-shaping rules and synthetic side channels.
```

Track:

```text
bedrock_request_metadata_validation
bedrock_web_search_options_grounding
bedrock_invoke_schema_in_last_user_message
bedrock_grounding_citation_annotation_rewrite
```

## Bedrock IAM credential caching varies by auth source

Bedrock IAM credential handling uses a process-wide in-memory cache with
different TTL behavior depending on auth source: static access-key credentials
are cached longer, ambient environment credentials use the cache default TTL,
and AssumeRole, web identity, profiles, and session-token tuples are
intentionally not cached so refresh state does not bleed across logical
sessions.

Design rule:

```text
Credential caching policy must be keyed by auth source, not applied uniformly.
```

Track:

```text
bedrock_iam_credential_cache_ttl
bedrock_iam_static_key_cache
bedrock_iam_assume_role_no_cache
```
