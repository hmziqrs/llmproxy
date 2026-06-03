# Mini Quirks — Provider Quirks: OpenAI-Compatible & Misc (2/3)

## Transport selection can force IPv4

The custom HTTP transport layer has a proxy-wide `force_ipv4` switch. When it
is enabled, LiteLLM builds an `AsyncHTTPTransport` with `local_address`
forced to `0.0.0.0` to avoid IPv6-related `httpx.ConnectionError` failures.
This is a transport policy, not a model or provider setting.

Design rule:

```text
Network transport quirks belong in transport policy, not in provider adapters.
```

Track:

```text
force_ipv4
transport_local_address
ipv6_connection_error_workaround
```

## Structured output is not one format

The refs map `response_format` differently depending on provider:
`json_object` may become `"json"`, `json_schema` may become a raw schema,
and Anthropic `output_config.format` may become OpenAI `response_format`.

Design rule:

```text
Structured output needs its own normalized capability, not just provider_meta.
```

Track:

```text
structured_output_mode = json_object | json_schema | text_schema | provider_native
schema_strictness = strict | best_effort | unsupported
schema_location = response_format | format | output_config | provider_specific
```

## Provider-specific headers are policy-scoped

LiteLLM supports extra headers scoped to one or more providers. These are
not equivalent to client request headers; they are upstream-rendering policy.

Design rule:

```text
Inbound headers, proxy policy headers, and upstream provider headers are separate maps.
```

Keep:

```text
client_headers
proxy_control_headers
provider_extra_headers
response_debug_headers
```

Never forward client headers wholesale to upstream providers.

## Endpoint families need separate cores

Embeddings, token counting, file upload, audio transcription, image
generation, and chat completion share auth/routing/cost concerns, but their
request and response invariants are different.

Design rule:

```text
Do not force non-chat endpoints through CoreChat.
```

Use endpoint-family cores:

```text
CoreChat
CoreEmbedding
CoreTokenCount
CoreFile
CoreImage
CoreAudio
CoreRerank
```

They can share provider routing, auth, headers, usage, and error handling,
but each needs its own normalized request/response/event model.

## Client error reporting should hide normal churn

The rotating-key reference reports abnormal credential errors like 401/403
with masked credential details, but summarizes normal operational errors
like 429/5xx unless every credential fails.

Design rule:

```text
Error detail level depends on operator actionability.
```

Separate:

```text
client_error_body
operator_log_body
credential_error_record
normal_error_summary
```

Mask API keys, OAuth file paths, and emails before they enter client-visible
errors or logs.

## Multipart endpoints need header surgery

WatsonX audio transcription removes `Content-Type` so the HTTP client can
set multipart boundaries automatically. It also sends project/space IDs as
form fields, not query params.

Design rule:

```text
Multipart rendering controls headers and field placement.
```

Track per endpoint:

```text
content_type_strategy = explicit_json | multipart_auto | binary_stream
form_fields
file_fields
query_fields
headers_to_remove
```

Do not globally set `Content-Type: application/json` for all upstream calls.

## Interactions and Responses are bridge protocols

The Interactions API bridge transforms Google-style turns into Responses
input, maps `system_instruction` to `instructions`, preserves selected
generation config, and then rebuilds Interactions output/steps from a
Responses response. It also keeps legacy `outputs` and new `steps` side by
side so older and newer callers can both work.

The Responses guardrail bridge does a different kind of rewrite: it extracts
function and MCP tools into a Chat Completions-shaped list for guardrails,
applies guardrail output, then remaps the tools back to Responses format while
preserving `web_search` and `web_search_preview` entries untouched.

Design rule:

```text
Bridge protocols should preserve both legacy and current shapes until migration is complete.
```

Core bridge state:

```text
bridge_input
bridge_output
legacy_output_view
current_output_view
bridge_passthrough_params
```

## Ollama strips provider prefixes before model lookup

Ollama model info lookups normalize the model string first. The adapter strips
`ollama/` and `ollama_chat/` prefixes before hitting `/api/show`, and the
model catalog logic checks multiple prefix variants so lookup, listing, and
cost-map matching all point at the same underlying model.

Ollama embeddings also repair missing usage provenance: they use the provider
`prompt_eval_count` when present, estimate prompt tokens from the local
encoding when that field is missing, and fall back to `0` only if no encoding
is available.

Design rule:

```text
Model identity normalization should happen before provider metadata lookup.
```

Track:

```text
ollama_strip_model_prefix
ollama_model_lookup_variants
ollama_model_info_resolution
ollama_prompt_eval_count_fallback
```

## Role alternation is a hidden prompt rewrite

The prompt-template code inserts default user/assistant continue messages
when a provider or model requires alternating roles. It can also merge a
system prompt into the next message or convert a trailing system prompt
into a user message.

Design rule:

```text
Role alternation is prompt normalization, not cosmetic formatting.
```

Track:

```text
role_sequence_before
role_sequence_after
continuation_placeholder_inserted
system_prompt_merged
system_prompt_rewritten_to_user
```

This is a real semantic rewrite and should be traceable.

## Prompt templates are model-family adapters

The template factory contains family-specific prompt grammars such as
Alpaca, Llama2, Falcon, MPT, WizardCoder, Phind, and Claude-compatible
formats. Some templates are intentionally lossy or fail closed when the
provider shape does not fit.

Design rule:

```text
Prompt templates are family-specific renderers, not generic string joins.
```

Capability fields:

```text
prompt_template_family
supports_system_role
supports_alternating_roles
supports_tool_sections
supports_xml_tool_results
supports_prefill_continuation
```

## cache_control is stripped and reshaped per provider family

The same `cache_control` field is not stable across bridges:

- Bedrock strips `scope` and may remove `ttl` unless the Claude 4.5 Bedrock shape is allowed
- Anthropic experimental pass-through strips `scope` on some message paths
- summary/compaction paths drop `cache_control` entirely because the summarizer does not need it

So `cache_control` is not a single portable token. It is a provider-specific control surface that may survive, shrink, or disappear depending on the route.

Design rule:

```text
Cache-control must be normalized by provider family, not copied blindly.
```

Track:

```text
cache_control_scope
cache_control_ttl
cache_control_preserved
cache_control_path = message | tool | summary | bedrock | anthropic_pass_through
```

## Provider-specific fields must survive the bridge

Some bridge paths preserve `provider_specific_fields` on both the function
chunk and the tool-call chunk so downstream consumers can recover data that the
core schema does not model. That is a hidden metadata channel, not a cosmetic
extra.

Design rule:

```text
Bridge protocols must preserve opaque provider fields end-to-end.
```

Track:

```text
provider_specific_fields
bridge_side_channel
roundtrip_metadata
```

## DeepSeek thinking mode is history-driven and can be forcibly disabled

The Go proxy does more than forward a `thinking` field. It inspects the
entire assistant history to decide whether thinking mode is already active.
If a DeepSeek request has assistant history but no thinking blocks, it sends
`thinking: {"type":"disabled"}` to override DeepSeek's default thinking mode
and prevent the next turn from requiring `reasoning_content`.

It also treats inline `thinking` attached to a `tool_use` block as real
thinking history, not as an optional annotation.

Design rule:

```text
Thinking mode is a conversation state, not just a request parameter.
```

Track:

```text
thinking_state = enabled | disabled | inherited | explicit
thinking_history_present
inline_tool_use_thinking
deepseek_safety_override
thinking_budget_tokens_normalized
```

## Nested drop params are applied before provider transforms

The custom HTTPX handler does not wait until the provider request is fully
built before removing unsupported fields. It strips nested paths from
`anthropic_messages_optional_request_params` up front, before the provider
transform runs. `additional_drop_params` is a pre-transform policy knob, not a
generic JSON cleanup after serialization.

Design rule:

```text
Drop policies must run on the pre-transform request shape.
```

Track:

```text
additional_drop_params
nested_drop_path
pre_transform_sanitization
```

## ChatGPT backend requests are force-shaped, not pass-through

The ChatGPT provider adapter does not simply forward Responses API knobs.
It forces `store = False`, forces `stream = True`, injects
`reasoning.encrypted_content` into `include`, and then drops every request key
outside a small allowlist. The caller's request is translated into the
provider's internal contract, not merely normalized.

Design rule:

```text
Provider adapters may intentionally narrow the request surface.
```

Track:

```text
forced_stream
forced_store_false
forced_include_reasoning_encrypted_content
request_key_allowlist
```

## MCP auto-execute rewrites a single request into a staged two-call flow

The MCP chat-completions path is not a straight proxy. It first transforms MCP
tools into OpenAI-shaped tools, then decides whether tool calls should be
auto-executed. If auto-execute is enabled, the initial call is forced to
`stream=false` even when the caller asked for streaming, so tool calls can be
captured and executed before the follow-up completion runs.

In streaming mode, the proxy does not simply forward chunks. It collects the
initial stream, reconstructs the complete response, extracts tool calls, runs
those tools, and then creates a second follow-up stream with the tool results.
The final user-visible iterator is therefore a stitched stream with a hidden
phase boundary.

MCP metadata is also treated as out-of-band state. Non-stream responses carry
`mcp_list_tools`, `mcp_tool_calls`, and `mcp_call_results` in
`provider_specific_fields`. Stream responses store the same data in
`_hidden_params["mcp_metadata"]` so the final chunk can be annotated later.

Design rule:

```text
MCP is a request rewrite plus a two-phase execution bridge, not a pass-through tool shim.
```

Track:

```text
mcp_hidden_metadata
auto_execute_forces_nostream
follow_up_stream_splice
tool_choice_removed_on_follow_up
```

## A2A agent cards are capability-filtered proxy surfaces

The A2A agent-card merge code does not expose the upstream card as-is. It
replaces the security schemes with a LiteLLM bearer scheme, rewrites the public
URL to the proxy URL, and filters capabilities down to an allowlist. Fields
like `pushNotifications`, `extendedAgentCard`, and `extensions` are not merely
ignored; they are intentionally dropped so the proxy does not advertise
behavior it cannot reliably serve.

It also preserves a small set of proxy-specific compatibility fields such as
`supportedInterfaces` and `url` for runtime use, while stripping alternate
backend entrypoints that would bypass proxy auth and logging.

Design rule:

```text
Published agent cards must describe the proxy surface, not the upstream backend.
```

Track:

```text
a2a_capability_allowlist
proxy_rewritten_security_scheme
proxy_url_rewrite
alternate_interface_suppression
```

## Databricks chat rewrites request shape before it ever reaches the model

Databricks chat does not forward request state as-is. It pops custom user-agent
fields from `optional_params` so they can be used in telemetry headers without
being sent upstream, strips empty text content because Databricks rejects it,
and moves message-level `cache_control` into a text content block when the
message content is a plain string.

For Claude models, `response_format` is converted into a tool-call bridge and
then removed from the request body. If streaming is requested with
`response_format`, the adapter forces a fake-stream path because Databricks
does not support that shape natively.

Design rule:

```text
Databricks chat is a request-shaping bridge, not an OpenAI pass-through.
```

Track:

```text
databricks_user_agent_header_split
databricks_empty_content_sanitizer
databricks_cache_control_block_lift
databricks_claude_json_mode_bridge
databricks_fake_stream_for_response_format
```
