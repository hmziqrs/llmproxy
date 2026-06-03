# Mini Quirks — Provider Quirks: Anthropic

## Anthropic-only extras must not leak after translation

LiteLLM’s Anthropic pass-through adapter consumes Anthropic-shaped fields
like `output_config`, translates meaningful parts to OpenAI-style params,
then prevents the raw Anthropic-only keys from leaking into non-Anthropic
backends. Otherwise strict providers return 400s or receive conflicting
duplicate fields.

Design rule:

```text
After translation, remove source-protocol-only fields unless explicitly preserved in ProviderMeta.
```

This is separate from dropping unsupported user intent. The translator
should record:

```text
ConsumedSourceField
ForwardedTargetField
SuppressedSourceOnlyField
ConflictingDuplicatePrevented
```

## Anthropic history repair strips invalid thinking and empty text

Anthropic request history is not always replayable as-is. The common-utils
repair path removes `thinking` and `redacted_thinking` blocks when a signature
no longer validates, and it strips empty or whitespace-only text blocks because
native Messages rejects them. Entire messages are dropped if their content
becomes empty after repair.

Design rule:

```text
History repair can delete content blocks to make a replayable request.
```

Track:

```text
anthropic_strip_thinking_blocks
anthropic_strip_empty_text_blocks
anthropic_drop_empty_repaired_messages
anthropic_invalid_thinking_signature_recovery
```

## SAP Anthropic JSON may arrive wrapped in markdown

SAP GenAI Hub sometimes returns Anthropic JSON responses wrapped in
markdown code fences like ```json ... ```. The adapter strips that wrapper
only when JSON mode is active, so non-JSON text responses stay untouched.

Design rule:

```text
Provider-specific output repair should be gated by the requested response mode.
```

Track:

```text
sap_strip_markdown_json
sap_anthropic_json_wrapped_output
sap_json_mode_only_repair
```

## Vertex AI Claude strips unsupported `output_config` keys

Vertex AI Claude routes share a leaf sanitizer that mutates `output_config`
in place before dispatch. Non-dict values are dropped entirely. Unsupported
keys are filtered out. If the remaining dict is empty, the field is removed
instead of being sent as `{}`. The companion `output_format` field is left
untouched.

This is a compatibility shim plus a CodeQL-cycle workaround, not a semantic
preservation step.

Design rule:

```text
Provider-specific output config needs a sanitization layer before dispatch.
```

Track:

```text
vertex_output_config_sanitizer
drop_non_dict_output_config
strip_unsupported_output_config_keys
remove_empty_output_config
```

## Bedrock caps Claude Opus `output_config.effort` to the provider ceiling

Bedrock does not always accept the full Anthropic `output_config.effort`
vocabulary for Claude Opus variants. The helper normalizes the requested
effort in place and clamps it to the model-specific ceiling from the Bedrock
model info map, which lets higher-level callers pass `xhigh`-style input
without sending a provider-invalid value upstream.

Design rule:

```text
Provider-specific effort levels can be rewritten downward to match the
published Bedrock ceiling.
```

Track:

```text
bedrock_opus_output_config_effort_cap
bedrock_output_config_effort_ceiling
bedrock_effort_value_clamp
```

## Anthropic experimental pass-through has its own context polyfills

The experimental Anthropic path does not treat `context_management` as a dumb pass-through.
It normalizes both Anthropic-native dict specs and OpenAI-style list specs, then applies
edits in order through a registry. The `clear_tool_uses_20250919` polyfill is especially
opinionated:

- only `trigger` and `keep` are honored in v0
- `clear_at_least`, `exclude_tools`, and `clear_tool_inputs` are ignored with warnings
- tool-use IDs are collected in chronological order
- the most recent completed `tool_result` is never cleared
- cleared tool results are replaced with placeholder content instead of being deleted

That means context management is a real state-editing protocol, not a generic message filter.

Design rule:

```text
Context edits must preserve the last valid reply boundary.
```

Track:

```text
context_edit_type = clear_tool_uses | compact | unknown
clear_tool_use_keep_count
clear_tool_use_ignored_knobs
last_completed_tool_result_id
cleared_tool_result_placeholder
```

## Anthropic legacy thinking gets rewritten into adaptive thinking

The Anthropic messages pass-through adapter does not leave legacy
`thinking.type=enabled` alone on adaptive-thinking models. It converts that
shape into `thinking: {"type": "adaptive"}` and derives an `output_config`
`effort` floor from the requested budget: larger budgets become higher effort
levels, and the caller's explicit `output_config.effort` still wins if it is
already set.

Design rule:

```text
Legacy feature flags can be translated into a newer provider-native mode
before the request ever reaches the wire.
```

Track:

```text
anthropic_legacy_thinking_to_adaptive
anthropic_adaptive_effort_from_budget
anthropic_output_config_effort_preserve
```

## Azure Anthropic promotes extra-body fields, rewrites auth headers, and strips unsupported request keys

Azure Anthropic is not just the generic Anthropic flow with a different host.
The adapter first promotes Anthropic-native fields out of `extra_body` so they
reach the request body directly, then applies Azure authentication while still
keeping Anthropic headers such as `anthropic-version` in place. It accepts the
Azure `api-key` or Azure AD token path, but still builds the Anthropic header
set for feature gating like prompt caching, computer use, PDF use, file IDs,
and MCP server usage. On the request side it removes Azure-unsupported
top-level fields such as `extra_body`, `max_retries`, and `stream_options`
before dispatch.

Design rule:

```text
When a provider exposes a hybrid Anthropic surface, Anthropic-native fields must be promoted before Azure-specific cleanup runs.
```

Track:

```text
azure_anthropic_promote_extra_body
azure_anthropic_azure_auth_with_anthropic_headers
azure_anthropic_strip_extra_body_max_retries_stream_options
azure_anthropic_version_header_preserved
```

## Azure Anthropic count-tokens uses a separate endpoint and combines Anthropic beta headers with Azure auth

Azure Anthropic token counting is its own request surface, not a chat
round-trip. The handler validates the messages with Anthropic’s count-tokens
logic, transforms the request into Anthropic’s count-tokens body, then posts to
`/anthropic/v1/messages/count_tokens`. Authentication is hybrid: it sends the
Anthropic-required `x-api-key`, `anthropic-version`, and `anthropic-beta`
headers, then overlays Azure auth headers from the Azure environment helper so
either `api-key` or `Authorization` can satisfy the provider. The response is
returned as Anthropic-compatible JSON without an extra transformation layer.

Design rule:

```text
Token-counting endpoints can have their own route and header contract even when they reuse the parent chat schema.
```

Track:

```text
azure_anthropic_count_tokens_endpoint
azure_anthropic_count_tokens_x_api_key_and_azure_auth
azure_anthropic_count_tokens_anthropic_beta_header
azure_anthropic_count_tokens_passthrough_response
```

## Azure Anthropic messages strips unsupported cache-control scope and rewrites api-key headers

Azure Anthropic messages is not just the normal Anthropic messages bridge with
Azure auth bolted on. The adapter converts Azure `api-key` headers into
`x-api-key` when needed, keeps `anthropic-version` and `anthropic-beta`
headers in the request, and then walks both the `system` blocks and the
message content blocks to remove `scope` from every `cache_control` object.
That means Azure Anthropic keeps prompt-caching semantics only in the subset
that the backend accepts, while the broader cache-control shape still looks like
Anthropic on the wire.

Design rule:

```text
Hybrid Anthropic providers need header translation plus provider-specific pruning of nested cache metadata.
```

Track:

```text
azure_anthropic_messages_x_api_key_conversion
azure_anthropic_messages_preserve_anthropic_beta
azure_anthropic_messages_cache_control_scope_strip
azure_anthropic_messages_system_and_message_block_walk
```

## Vertex partner Anthropic messages rewrites the Anthropic version, synthesizes beta headers, and strips unsupported output params

Vertex partner Anthropic messages is a separate bridge from the generic Vertex
Anthropic path. It computes a partner-specific `api_base` from the Vertex
project/location pair, injects Vertex bearer auth when the caller did not
already provide authorization, and always rewrites the request body to use
`anthropic_version="vertex-2023-10-16"`. The adapter also removes `model` from
the body, sanitizes unsupported `output_config` values, and synthesizes the
correct `anthropic-beta` header set from the active tools, context-management
edits, and tool-search/web-search capabilities.

Design rule:

```text
Partner model wrappers may need their own Anthropic dialect, beta-header synthesis, and body cleanup even when the base provider is already Anthropic-like.
```

Track:

```text
vertex_partner_anthropic_version_vertex_dialect
vertex_partner_anthropic_beta_header_synthesis
vertex_partner_anthropic_model_body_strip
vertex_partner_anthropic_output_param_sanitization
vertex_partner_anthropic_authorization_fallback
```

## Anthropic OAuth tokens rewrite auth headers and force a browser-access beta flag

Anthropic OAuth is not carried as a normal API key. When the request already
contains a bearer token that starts with `sk-ant-oat`, the helper removes
`x-api-key`, rewrites the request to `authorization: Bearer ...`, merges the
Anthropic OAuth beta header into any existing beta list, and sets
`anthropic-dangerous-direct-browser-access=true`. The same header rewrite
happens when the token is passed in directly as `api_key`, so the auth shape is
normalized before the request leaves the proxy.

Design rule:

```text
OAuth-backed Anthropic calls need a different auth/header envelope than standard x-api-key requests.
```

Track:

```text
anthropic_oauth_bearer_rewrite
anthropic_oauth_beta_merge
anthropic_oauth_direct_browser_access_flag
anthropic_oauth_x_api_key_removal
```

## Anthropic pass-through to Responses rewrites thinking, structured output, and user identity across the bridge

The Anthropic experimental Responses adapter is a real schema bridge, not a
thin relay. On the request path it turns Anthropic `thinking` into Responses
`reasoning` with effort derived from `budget_tokens` or `output_config.effort`,
maps Anthropic JSON-schema output into Responses `text.format`, converts
`context_management` edits into the Responses compaction array, and truncates
Anthropic `metadata.user_id` into the Responses `user` field. On the response
path it reverses the process: Responses reasoning summaries become Anthropic
`thinking` blocks, output text becomes Anthropic text blocks, function calls
become Anthropic `tool_use`, and incomplete Responses statuses are normalized
back to Anthropic `stop_reason="max_tokens"`.

Design rule:

```text
Bridges between protocol families have to translate both the request contract and the terminal-state contract.
```

Track:

```text
anthropic_responses_thinking_to_reasoning
anthropic_responses_output_format_bridge
anthropic_responses_context_management_bridge
anthropic_responses_user_id_truncation
anthropic_responses_summary_to_thinking
anthropic_responses_incomplete_to_max_tokens
```
