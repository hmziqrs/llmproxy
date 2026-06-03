# Mini Quirks — Tool Calls & Tool Schemas

## Partial tool-call JSON must be stateful

Some adapters accumulate tool-call argument fragments by index and wait
until the JSON becomes parseable before emitting a target-protocol tool
call. Others emit raw partial JSON deltas directly.

Design rule:

```text
Tool-call streaming has two valid modes: delta mode and assembled mode.
```

Expose this as a target protocol capability:

```text
supports_tool_call_delta = true | false
requires_parseable_tool_args_before_emit = true | false
```

If the target requires assembled calls, the adapter needs per-tool state:

```text
tool_index
tool_id
name_fragments
argument_fragments
json_parse_state
```

## Determinism knobs can hurt tool use

One reference has an environment-controlled override for `temperature=0`,
either removing the field or rewriting it to `1.0`, because deterministic
outputs can cause schema/tool hallucination for some clients and models.

Design rule:

```text
Generation policy rewrites must be explicit proxy policy, not hidden normalization.
```

Core should separate:

```text
client_requested_sampling
proxy_policy_sampling
provider_rendered_sampling
```

If the proxy changes `temperature`, `top_p`, `seed`, or similar knobs, emit
a policy warning/tracing record. This is not a pure protocol translation.

## Anthropic tool names are rewritten per request

Anthropic requires tool names to match `^[a-zA-Z0-9_-]{1,128}$`, so the
adapter rewrites invalid names and resolves collisions with numeric suffixes.
It also keeps a reverse map only for rewritten names, so a valid tool name
that happens to match another tool's sanitized form is not translated back
incorrectly on the response path.

Design rule:

```text
Tool-name sanitization must be collision-safe and reversible only where needed.
```

Track:

```text
anthropic_tool_name_sanitization
anthropic_tool_name_collision_suffix
anthropic_tool_name_reverse_map
anthropic_tool_name_reserved_hosts
```

## Anthropic JSON mode strips internal response-format tool calls

In Anthropic non-streaming JSON mode, LiteLLM treats `response_format` tool
calls as internal plumbing. If every tool call is the internal
`response_format` tool, it converts that payload back into a message and
drops the tool list entirely. If user tools are mixed in, it removes only the
internal tool calls and merges the JSON payload into content so the caller
still sees the structured result.

Design rule:

```text
Internal structured-output tools must be collapsed back into content before the response leaves the adapter.
```

Track:

```text
anthropic_json_mode_strip_internal_response_format_tool
anthropic_json_mode_merge_payload_into_content
anthropic_json_mode_mixed_tool_filter
```

## OCI tool schemas are rewritten before dispatch

OCI adapters do not forward raw JSON Schema. They first resolve `$ref` and
`anyOf`, then sanitize the schema into an OCI-safe subset: `title` fields are
dropped, `default: null` entries are removed, nullable `type` lists are
collapsed, array schemas get an `items` field, and invalid `required` entries
are pruned.

Design rule:

```text
Provider-native tool schemas may need a structural rewrite before they are valid.
```

Track:

```text
oci_schema_resolve_refs
oci_schema_resolve_anyof
oci_schema_sanitize
oci_schema_items_inference
oci_schema_required_prune
```

## Mistral strips schema noise and tool-message extras

Mistral does not accept raw OpenAI tool schemas. The adapter removes schema
bits that trigger API errors before dispatch, and it also narrows message
shape on the way through: only tool messages may keep `name`, empty tool names
are removed, and empty assistant text is treated as invalid noise.

Design rule:

```text
Provider adapters may need to delete fields, not just rename them.
```

Track:

```text
mistral_tool_schema_cleaning
mistral_tool_name_scrub
mistral_empty_assistant_text_drop
```

## Bedrock strips Claude Code custom tool fields

Bedrock rejects Claude Code's `tool.custom` payload even though Anthropic
accepts it, so the adapter removes `custom` from tool definitions before
dispatch. The same Bedrock utility also rewrites `input_schema.type:
"custom"` into `object` so Claude Code tool schemas become valid JSON Schema
for Bedrock Invoke and Converse.

Design rule:

```text
Provider adapters sometimes need to strip or coerce tool metadata for backend compatibility.
```

Track:

```text
bedrock_strip_tool_custom_field
bedrock_custom_schema_type_to_object
bedrock_claude_code_tool_cleanup
```

## Azure chat gates `tool_choice` by API version

Azure chat completion routes do not treat `tool_choice` as a static OpenAI
param. The adapter checks the Azure API version first, and older preview
versions reject `tool_choice` entirely. Even on newer previews, the value
`required` is blocked on older 2024 preview versions unless the caller opts
into dropping unsupported params.

Design rule:

```text
API-version-gated params need explicit drop policy, not optimistic passthrough.
```

Track:

```text
azure_chat_tool_choice_version_gate
azure_chat_tool_choice_required_gate
azure_chat_drop_unsupported_params
```

## Tool-call repair belongs in the prompt layer too

Prompt utilities attempt to repair truncated tool-call JSON, add warnings
when repair succeeds, and sanitize Anthropic `tool_use_id` values to match
the provider regex.

Design rule:

```text
Tool-call repair is part of prompt rendering, not just response parsing.
```

## Cohere v2 forces single-step mode when tool results sit behind user history

The Cohere v2 chat adapter rewrites `tool_results` into the current request,
but if the last entry in `chat_history` is a `USER` message it also injects
`force_single_step=True` because the upstream API fails otherwise.

Design rule:

```text
History tail can change the provider execution mode.
```

Track:

```text
cohere_force_single_step_on_user_tail
cohere_tool_results_history_dependency
```

## Vertex Anthropic forces a tool-based structured-output path by lying about the model

When `response_format` is present, the Vertex AI Anthropic adapter temporarily
swaps the model name to `claude-3-sonnet-20240229` before delegating to the
parent OpenAI-param mapper. That forces the structured-output path to go
through tool use instead of the native output-format route Vertex does not
support. The original model name is restored afterward for downstream
response shaping.

Design rule:

```text
If a provider lacks a feature, the adapter may select a different internal
model solely to force the right request translation path.
```

Track:

```text
vertex_anthropic_response_format_model_swap
vertex_anthropic_structured_output_tool_path
vertex_anthropic_original_model_restore
```

## xAI Responses strips unsupported top-level fields and rewrites tool definitions

The xAI Responses adapter drops
`instructions` and `metadata` entirely, removes the `container` field from
`code_interpreter` tools, rewrites `web_search` into xAI's `filters` shape,
and maps `x_search` into xAI's native tool schema. The endpoint also does not
offer native WebSocket transport, so the bridge is HTTP-only even though the
surface looks Responses-compatible.

Design rule:

```text
Responses compatibility does not imply field compatibility. Unsupported
top-level knobs and tool schemas need explicit adapter-side rewrites or drops.
```

Track:

```text
xai_responses_drop_instructions
xai_responses_drop_metadata
xai_responses_code_interpreter_container_strip
xai_responses_web_search_filter_rewrite
xai_responses_x_search_tool_rewrite
xai_responses_http_only
```

## Hosted vLLM rewrites custom tools and assistant thinking blocks into OpenAI-shaped content

The Hosted vLLM chat adapter strips tool
schemas down to OpenAI function tools when the caller sends `type: "custom"`,
so the upstream validation path only sees a function-style tool call. It also
rewrites assistant `thinking_blocks` into structured content blocks and can
convert video-bearing file items into `video_url` blocks.

Design rule:

```text
Adapters that accept richer local tool or content shapes may need to collapse
them into the narrower shapes a hosted OpenAI-compatible backend can validate.
```

Track:

```text
hosted_vllm_custom_tool_to_function_tool
hosted_vllm_thinking_blocks_to_content
hosted_vllm_video_file_to_video_url
```

## DeepInfra chat flattens tool messages and enforces a narrow `tool_choice` contract

In the DeepInfra chat adapter, tool messages
must be strings, so any array-shaped tool content is flattened or serialized
before dispatch. It also refuses `tool_choice` values other than `auto` and
`none` unless `drop_params` is enabled, in which case the unsupported value is
silently removed instead of being sent upstream.

Design rule:

```text
Adapters need an explicit policy for tool-message shape and unsupported tool
selection values, because not every OpenAI-compatible backend accepts the same
tool contract.
```

Track:

```text
deepinfra_tool_message_array_to_string
deepinfra_tool_choice_auto_none_only
deepinfra_tool_choice_drop_or_reject
```

## Ollama JSON mode can turn returned JSON into either a tool call or a structured message

Ollama's chat adapter does more than map prompt parameters. When the request
is in JSON mode, it inspects the returned `response` text and branches on the
content: a dict with `name` and `arguments` becomes a synthetic tool call
with `finish_reason="tool_calls"`, while any other valid JSON is re-emitted as
the assistant message content. If the payload is not valid JSON, the adapter
falls back to the plain text/`reasoning_content` path instead.

Design rule:

```text
When a provider overloads JSON mode, the adapter may need to infer whether the
payload is structured content or a tool-call envelope.
```

Track:

```text
ollama_json_mode_tool_call_inference
ollama_json_mode_structured_message_passthrough
ollama_json_mode_reasoning_fallback
```

## Snowflake rewrites tool definitions and response content lists into its own schema

On the request path, the Snowflake adapter converts OpenAI function tools into
Snowflake `tool_spec` objects and
rewrites `tool_choice` from OpenAI's string/dict shapes into Snowflake's
object format. On the response path, it collapses Snowflake `content_list`
items back into OpenAI `content` plus `tool_calls`, then strips the provider
specific `content_list` field from the returned message.

Design rule:

```text
Adapters for schema-heavy backends need explicit bidirectional translation for
both tool definitions and response content envelopes.
```

Track:

```text
snowflake_tool_spec_rewrite
snowflake_tool_choice_object_rewrite
snowflake_content_list_to_openai_message
snowflake_tool_call_reconstruction
```

## IBM WatsonX splits OpenAI tool choice into `tool_choice_option` and object form

IBM WatsonX does not accept OpenAI `tool_choice` as a single universal shape.
If the caller passes `auto`, `none`, or `required`, the adapter moves that
value into `tool_choice_option`. If the caller passes a function object, it is
preserved as `tool_choice` in object form. That split is part of the adapter
contract, not an incidental parameter rename.

Design rule:

```text
Some backends separate "pick a mode" from "pick a specific function", so the
adapter has to route OpenAI's tool choice into two different fields.
```

Track:

```text
watsonx_tool_choice_option_mode
watsonx_tool_choice_object_passthrough
watsonx_tool_choice_split
```

## Anthropic MCP server tools are rewritten into Anthropic URL tools with stripped bearer auth and tool allowlists

OpenAI-style MCP server tools do not stay in their original shape. The Anthropic
chat mapper converts them into Anthropic `type="url"` server tools, renames the
server URL into `url`, renames the label into `name`, carries any `allowed_tools`
into `tool_configuration.allowed_tools`, and strips a bearer token out of the
incoming `Authorization` header so it can be sent as `authorization_token`.
The mapper also accumulates these rewritten MCP server definitions separately
from normal tools and places them into `mcp_servers` for downstream header
synthesis and request shaping.

Design rule:

```text
Server-tool bridges must preserve the server identity, allowlist, and auth token in the provider-native shape.
```

Track:

```text
anthropic_mcp_server_url_tool_rewrite
anthropic_mcp_server_allowed_tools_bridge
anthropic_mcp_server_authorization_token_strip
anthropic_mcp_server_sidecar_list
```

## Anthropic tool mapping coerces external schemas into Anthropic's stricter tool shape

Anthropic does not accept arbitrary OpenAI tool schemas as-is. The mapper
rewrites `function` and `custom` tools into Anthropic `type="custom"` tools,
coerces any non-object `input_schema.type` to `object`, inserts an empty
`properties` map when needed, inlines legacy `$defs` before filtering, and then
drops any schema fields outside Anthropic's allowed input-schema set. It also
preserves `description`, `cache_control`, `defer_loading`, `allowed_callers`,
and `input_examples` only when the target tool type supports them, while
computer tools are rewritten into fixed display-dimension descriptors and
hosted tools keep their provider-specific extras.

Design rule:

```text
Tool translation has to normalize schema shape, not just rename fields.
```

Track:

```text
anthropic_tool_schema_object_coercion
anthropic_tool_schema_legacy_defs_inline
anthropic_tool_schema_allowed_field_filter
anthropic_tool_cache_control_passthrough
anthropic_tool_input_examples_passthrough
anthropic_computer_tool_dimension_bridge
anthropic_hosted_tool_extra_param_passthrough
```
