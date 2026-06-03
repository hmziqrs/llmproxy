# Mini Quirks — Provider Quirks: OpenAI-Compatible & Misc (3/3)

## OpenAI-like chat rewrites `max_completion_tokens` for generic compatibility

The OpenAI-like chat wrapper converts `max_completion_tokens` into
`max_tokens` before dispatch because most OpenAI-compatible providers only
understand the older field name. That means the outward request shape is
preserved only at the API boundary, not on the wire to the upstream model.

Design rule:

```text
Compatibility wrappers should normalize field names at the last possible hop.
```

Track:

```text
openai_like_max_completion_tokens_rewrite
openai_like_compat_field_mapping
```

## Predibase chat rewrites sampling knobs into Hugging Face inference fields

The Predibase adapter does not forward OpenAI sampling parameters directly.
It rewrites `n` into `best_of` and turns on `do_sample`, coerces `temperature`
from `0` to `0.01` because the upstream HF backend rejects zero, and rewrites
`max_tokens`/`max_completion_tokens` into `max_new_tokens` while bumping zero
up to `1`.

That same adapter also interprets `details.best_of_sequences` as extra choices
and reconstructs multiple completions from a single upstream response. The
result is a provider-specific output fan-out, not a single-response passthrough.

Design rule:

```text
Sampling knobs can change both request shape and response cardinality.
```

Track:

```text
predibase_temperature_zero_fallback
predibase_best_of_to_do_sample
predibase_max_new_tokens_floor
predibase_best_of_response_fanout
```

## Databricks Responses strips provider prefixes and stays HTTP-only

Databricks Responses removes a leading `databricks/` prefix from the model
name before delegating to the OpenAI Responses transformer. The adapter also
explicitly reports that Databricks does not support a native WebSocket
transport for this endpoint family, so the proxy has to stay on the HTTP path.

Design rule:

```text
Responses adapters should declare when transport support is HTTP-only and normalize provider prefixes before delegation.
```

Track:

```text
databricks_responses_model_prefix_strip
databricks_responses_http_only
databricks_responses_delegate_to_openai_transformer
```

## Volcengine Responses repairs missing `response.output` before validation

Volcengine's Responses stream is not fully self-describing. The adapter first
patches any `response.*` chunk that lacks `response.output` by inserting an
empty list, then fills any other missing fields with the event model defaults
before pydantic validation. That makes the stream tolerant of incomplete
provider frames instead of failing immediately on schema mismatch.

Design rule:

```text
Stream validators sometimes need repair logic before schema parsing.
```

Track:

```text
volcengine_response_output_backfill
volcengine_stream_field_repair
volcengine_model_defaults_fill
```

## ChatGPT subscription state is synthesized from auth and call metadata

The ChatGPT backend adapter does more than attach a bearer token. It derives
`account_id` from auth claims when needed, pulls or synthesizes `session_id`
from LiteLLM params and trace metadata, and sends those as explicit upstream
headers. It also prepends a provider-default instruction block before any
caller-supplied `instructions`, with an environment override for the default
text.

Design rule:

```text
Provider identity can come from auth claims, call metadata, and forced prompt
prefixes, not just the model string.
```

Track:

```text
chatgpt_account_id_from_auth
chatgpt_session_id_from_call_metadata
chatgpt_default_instructions_prefix
chatgpt_identity_headers
```

## Mistral response content lists are collapsed into text and reasoning content

The Mistral chat adapter does more than strip schema noise on the way in. On
the way back, it rewrites empty assistant content from `""` to `null`, and if
the provider returns a content list it collapses that list into plain text.
`thinking` blocks are extracted into `reasoning_content`, while `text` blocks
become the visible assistant message. That means the adapter is not preserving
the provider response shape verbatim; it is synthesizing the OpenAI-shaped
output the rest of the stack expects.

Design rule:

```text
When a provider uses structured content blocks, the bridge may need to split
visible text from reasoning text before it can present a normalized response.
```

Track:

```text
mistral_empty_content_to_null
mistral_content_list_to_text_and_reasoning_content
mistral_thinking_block_extraction
```

## Vercel AI Gateway nests provider-specific options under `extra_body`

The Vercel AI Gateway adapter keeps the OpenAI-shaped request surface, but it
does not send provider-specific fields as normal top-level parameters. Any
`providerOptions` block from the caller is moved into `extra_body`, because
that is the only place the upstream OpenAI client can carry opaque gateway
metadata through. The adapter also allows an OIDC token fallback for auth, so
the effective credential source can differ from the nominal `api_key` input.

Design rule:

```text
If the transport is OpenAI-compatible but the gateway needs its own knobs,
those knobs should be isolated in the extra-body escape hatch instead of
polluting the shared parameter surface.
```

Track:

```text
vercel_ai_gateway_provider_options_extra_body
vercel_ai_gateway_oidc_token_fallback
```

## Baseten switches base URLs when the model name looks like a dedicated deployment

Baseten does not treat every model string the same way. If the model ID is an
8-character alphanumeric deployment token, the adapter rewrites the request
to the dedicated deployment host under `model-{id}.api.baseten.co`. Otherwise
it stays on the shared `inference.baseten.co/v1` API. That means the model
string is both a selector and a routing signal.

Design rule:

```text
Some providers encode deployment identity in the model name itself, so the
adapter has to branch the upstream base URL before any request is sent.
```

Track:

```text
baseten_dedicated_deployment_model_id
baseten_model_to_deployment_host_rewrite
baseten_shared_vs_dedicated_api_base
```

## Novita injects a source header into every request

The Novita chat adapter is otherwise OpenAI-compatible, but it still mutates
the request headers before dispatch. In addition to the bearer token and
JSON content type, it always adds `X-Novita-Source: litellm`. That is a
provider-level attribution requirement, not just generic transport plumbing.

Design rule:

```text
Some providers require an adapter-owned source marker in addition to normal
auth headers, and that marker has to be treated as part of the contract.
```

Track:

```text
novita_source_header_litellm
novita_request_attribution_header
```

## LM Studio fabricates an API key so the OpenAI client layer stays happy

LM Studio does not require a real API key, but the OpenAI client path still
expects a non-`None` credential. The adapter therefore falls back to a fake
`fake-api-key` when neither the caller nor environment provides one. That is a
local-runtime compatibility shim, not a real auth credential.

Design rule:

```text
Some local OpenAI-compatible servers need a placeholder key solely because
the client library refuses to operate without one.
```

Track:

```text
lm_studio_fake_api_key_fallback
lm_studio_openai_client_placeholder_key
```

## LM Studio rewrites flat `schema` into `response_format.json_schema`

LM Studio also normalizes the OpenAI structured-output payload. If the caller
sends `response_format.type == "json_schema"` with a top-level `schema`
field, the adapter wraps that schema under
`response_format.json_schema.schema` before dispatch. That lets the caller use
a flatter schema shape while still sending the nested form the server expects.

Design rule:

```text
Local OpenAI-compatible runtimes may accept a friendlier structured-output
shape, but the adapter still needs to reshape it into the server's nesting.
```

Track:

```text
lm_studio_response_format_schema_wrap
lm_studio_json_schema_nested_conversion
```

## Heroku flattens list-shaped messages and auto-appends the chat-completions path

The Heroku inference adapter accepts OpenAI-shaped input, but it still needs
adapter-side normalization. It collapses list-shaped message content into
strings because Heroku does not support array content, and it appends
`/v1/chat/completions` to the base URL when the caller omits the full path.
That makes both the message format and the endpoint path part of the adapter
contract.

Design rule:

```text
Even when a provider is OpenAI-compatible, the adapter may still need to
normalize content shape and finalize the endpoint path.
```

Track:

```text
heroku_content_list_to_string
heroku_chat_completions_path_append
heroku_base_url_required
```

## DeepInfra special-cases `temperature=0` for one model

The DeepInfra chat adapter does not treat `temperature` as a pure pass-through
for every model. For `mistralai/Mistral-7B-Instruct-v0.1`, a literal zero is
rewritten to `MIN_NON_ZERO_TEMPERATURE` because the upstream model rejects
`temperature=0`. That is a model-specific compatibility shim, not a generic
sampling policy.

Design rule:

```text
Sometimes a provider adapter has to carry a model-specific workaround for one
sampling knob so the request can survive upstream validation.
```

Track:

```text
deepinfra_temperature_zero_min_non_zero
deepinfra_model_specific_temperature_workaround
```

## GigaChat rewrites temperature zero and synthesizes structured output via function calling

GigaChat does not pass OpenAI sampling and structured-output settings through
verbatim. A literal `temperature=0` becomes `top_p=0`, and `response_format`
with `json_schema` is converted into a synthetic function definition plus a
forced `function_call` for that generated name. The adapter also maps OpenAI
`tool_choice` into GigaChat's `function_call` shape, so the request body is
being actively reshaped before dispatch.

Design rule:

```text
If a backend exposes structured output through function calling, the adapter
has to synthesize that function layer from the caller's schema.
```

Track:

```text
gigachat_temperature_zero_to_top_p_zero
gigachat_structured_output_to_function_call
gigachat_tool_choice_to_function_call
```

## Together AI text completions require a single string prompt

Together AI's text-completion adapter does not accept the OpenAI prompt list
shape blindly. It collapses the conversation into one string, rejects integer
token inputs outright, and raises if the caller tries to send multiple prompt
strings. That means prompt aggregation itself is part of the adapter
contract.

Design rule:

```text
Text-completion adapters for providers that want a single prompt string need
to reject multi-prompt and token-list inputs explicitly.
```

Track:

```text
together_ai_single_string_prompt
together_ai_reject_multi_prompt
together_ai_reject_integer_prompt_input
```

## IBM WatsonX converts chat messages into model-family prompt templates and switches endpoints for deployments

WatsonX chat is not a direct chat-completions passthrough. The adapter turns
chat messages into a single prompt string using model-family-specific
templates, then builds either the shared chat endpoint or a deployment-
specific endpoint depending on whether the model name starts with
`deployment/`. It also accepts request-level `api_version` overrides after the
base URL is assembled, so route construction is part of the adapter behavior
instead of a static config detail.

Design rule:

```text
When a provider is actually a prompt-template bridge, both the prompt renderer
and the endpoint selector are part of the protocol contract.
```

Track:

```text
watsonx_prompt_template_by_model_family
watsonx_deployment_endpoint_switch
watsonx_api_version_post_route_override
```

## Perplexity Responses repairs request shape and treats HTTP 200 failure bodies as errors

Perplexity Responses is not a straight OpenAI Responses passthrough. The
adapter injects `type="message"` into list input items that omit a type,
rewrites `model="preset/..."` into a `{ "preset": ... }` request body, and
then rejects HTTP 200 responses whose JSON body says `status="failed"`.

Design rule:

```text
Responses adapters need to normalize request intent and inspect body-level
failure states, not just HTTP status codes.
```

Track:

```text
perplexity_responses_inject_message_type
perplexity_responses_preset_model_body
perplexity_responses_body_status_failed_error
```

## Nvidia NIM chat uses model-specific parameter allowlists instead of one shared OpenAI surface

Nvidia NIM chat is only superficially OpenAI-compatible. The adapter changes
its supported-parameter list by model family: some Gemma models allow only a
small core set, `nvidia/nemotron-4-340b-instruct` adds `max_completion_tokens`,
`nvidia/nemotron-4-340b-reward` is basically stream-only, `google/codegemma`
has its own parameter subset, and the default branch exposes tools,
`tool_choice`, `parallel_tool_calls`, and `response_format`. The mapping
layer then rewrites `max_completion_tokens` to `max_tokens` before dispatch.

Design rule:

```text
Provider compatibility is not binary; model-specific allowlists can be part
of the adapter contract even when the host API looks OpenAI-shaped.
```

Track:

```text
nvidia_nim_chat_model_specific_param_allowlists
nvidia_nim_chat_reward_model_stream_only
nvidia_nim_chat_max_completion_tokens_to_max_tokens
nvidia_nim_chat_tools_response_format_default_branch
```

## GitHub Copilot chat rewrites system messages, classifies initiator state, and sets vision headers

GitHub Copilot chat is not a trivial OpenAI clone. The adapter converts
system messages to assistant messages unless system-to-assistant conversion is
explicitly disabled, classifies the request as `user` or `agent` initiated
based on message roles, injects the Copilot header bundle, and sets
`Copilot-Vision-Request: true` when any message contains image content. It
also treats Claude-model reasoning support as a model-specific parameter gate,
not a universal one.

Design rule:

```text
Even when an API looks OpenAI-shaped, the adapter may still need to rewrite
roles and classify request provenance before the backend will accept it.
```

Track:

```text
github_copilot_system_to_assistant_rewrite
github_copilot_initiator_classification
github_copilot_vision_header_chat
github_copilot_claude_reasoning_gate
```
