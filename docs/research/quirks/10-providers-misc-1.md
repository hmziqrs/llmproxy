# Mini Quirks — Provider Quirks: OpenAI-Compatible & Misc (1/3)

## Do not hide lossy paths

Some reference adapters are knowingly lossy:

- Anthropic images become `"[Image]"` in some OpenAI-chat paths.
- Gemini and Responses streams in `oc-go-cc` are mostly text-only.
- Some reverse translations reconstruct only text/thinking/tool_use.
- Metadata and unsupported fields can be dropped silently.

Design rule:

```text
If an adapter drops structure, emit a TranslationWarning.
```

Lossy capability should be declared:

```toml
[providers.some_provider.capabilities]
stream_text = true
stream_tools = false
stream_reasoning = false
preserves_multimodal_tool_results = false
```

## No replay after visible output

One audited stream path can still retry after bytes already escaped in
some error branches. That can duplicate content or corrupt tool-call
state.

Design rule:

```text
Once visible output escaped, fallback is disabled unless explicitly safe.
```

Track:

```text
has_visible_text
has_tool_delta
has_reasoning_only_delta
```

## Terminal state is not just stop

Some adapters collapse `response.completed`, `response.failed`, and
`response.incomplete` into the same final message flow. That hides
important terminal state.

Design rule:

```text
Core terminal events need terminal_state and stop_reason.
```

Example:

```rust
enum TerminalState {
    Completed,
    Incomplete,
    Failed,
    Canceled,
    TransportClosed,
}
```

## Null error codes are coerced so validation can pass

Some OpenAI-compatible providers send an `error` object with `code: null`.
The Responses transformation normalizes that to an explicit placeholder code
before Pydantic validation runs, because the schema expects a concrete value.

This is a response-shape repair, not a semantic rewrite of the upstream error.

Design rule:

```text
Repair invalid provider error payloads before validation, but preserve the error carrier.
```

Track:

```text
null_error_code_coercion
error_payload_validation_repair
unknown_error_placeholder
```

## Null `top_logprobs` are normalized to an empty list

Some OpenAI-compatible providers return `top_logprobs: null` when logprobs are
enabled but no top-logprobs payload is present. LiteLLM normalizes that to `[]`
so the typed list contract survives validation.

Design rule:

```text
Provider output should be coerced into the declared collection shape before validation.
```

Track:

```text
top_logprobs_null_to_empty
logprobs_shape_repair
typed_list_contract
```

## Null roles default to `assistant`

The OpenAI message model uses `role or "assistant"` when constructing
messages, so a missing/null role is coerced to `assistant` instead of failing
validation. That keeps downstream consumers from tripping over incomplete
assistant payloads.

Design rule:

```text
Missing roles should be normalized to the protocol's safest default before validation.
```

Track:

```text
null_role_default_assistant
message_role_repair
default_assistant_role
```

## Auth compatibility is a surface

Clients use different auth conventions even when the proxy key is the
same logical credential:

```text
Authorization: Bearer ...
x-api-key: ...
Basic ...
provider-specific OAuth
```

Design rule:

```text
Inbound auth normalization and upstream auth rendering are separate.
```

## Beta headers and feature flags

Anthropic beta headers and context-management knobs are not safe
passthrough fields. LiteLLM has a dedicated beta-header manager because
support varies by provider, model, and deployment. Some beta headers are
rewritten, some are unsupported, and some are loaded from provider config.

Design rule:

```text
Beta headers are capability-gated features, not raw forwarded headers.
```

Core should track the outcome:

```text
ForwardedHeader
RewrittenHeader
DroppedUnsupportedHeader
DroppedUnknownHeader
```

This matters for:

- Anthropic `context-management-*` beta headers.
- Interleaved thinking and fine-grained tool streaming.
- Provider-specific aliases for the same Anthropic-facing feature.

The beta-header path is backed by a fetched-and-cached provider mapping.
Unknown headers are dropped, unsupported headers are dropped, and provider
aliases are resolved before filtering.

Anthropic computer-tool versions are also normalized into beta-header names:
`computer_20250124` becomes `computer-use-2025-01-24`, `computer_20241022`
becomes `computer-use-2024-10-22`, and unknown versions fall back to the
older `computer-use-2024-10-22` header.

Design rule:

```text
Beta header forwarding must consult provider capability maps before emission.
```

Track:

```text
beta_header_registry
provider_alias_resolution
unsupported_beta_drop
unknown_beta_drop
computer_tool_beta_mapping
```

## Unsupported parameter policy

Multiple references silently drop unsupported parameters. That is convenient
for compatibility, but bad for a normalization layer because users cannot
tell whether a behavior was honored.

Design rule:

```text
No global silent drop_params mode.
```

Every request field should resolve to one of:

```text
Forwarded
Rewritten
DroppedWithWarning
Rejected
StoredInProviderMeta
```

Examples that need explicit policy:

- OpenAI `parallel_tool_calls` on providers that cannot enforce it.
- Anthropic `metadata` when translating to OpenAI Chat.
- Gemini-only thinking parameters on non-Gemini providers.
- Embedding/image dimensions on providers that reject dimensions.

Bytez is more rigid than a soft drop policy. It keeps an explicit alias map
from OpenAI params to Bytez params, marks unsupported fields with `False`,
and either drops or rejects them depending on `drop_params`. It also moves
`stream` out of the generic params bag and into a top-level request field.

GPT-5 chat routing has its own normalization rules. Dict-shaped
`reasoning_effort` is collapsed to the `effort` string, `max_tokens` is
rewritten to `max_completion_tokens`, `temperature` is only accepted in the
narrow supported cases, and `reasoning_effort="xhigh"` is treated as an
explicit capability gate rather than a soft hint.

## Message repair and sanitizer paths

References repair malformed or provider-hostile messages before sending
upstream: empty text blocks, invalid tool IDs, duplicate tool results,
orphaned tool results, empty tool schemas, and provider-specific ordering
rules.

Design rule:

```text
Repairs are explicit translation outcomes.
```

Core should distinguish:

```text
Original input
Normalized input
Provider-rendered input
Repair warnings
```

This keeps compatibility hacks testable instead of hiding them in
string-building code.

## Context management cannot be assumed

Responses API and Anthropic-style context management have provider-specific
behavior. Some providers support previous-response/container affinity, some
support cache-control-style hints, and some reject context-management fields
entirely.

Design rule:

```text
Context management is a capability with routing implications.
```

Do not silently drop it for protocols where the client expects continuity.
Either route to a capable provider, store the field in provider metadata
with a warning, or reject the request.

## Capability registry beats provider name checks

The audited code has model-name checks for DeepSeek, Kimi, Gemini,
Mistral, Qwen, Anthropic-native models, Responses models, and more.

Design rule:

```text
Provider quirks are capabilities, not scattered string checks.
```

Example:

```toml
[models.kimi]
requires_non_empty_reasoning_on_tool_calls = true
supports_required_tool_choice = false

[models.gemini_cli]
requires_thought_signature_cache = true
may_emit_usage_late = true
supports_context_management = false

[models.deepseek]
defaults_to_reasoning = true
requires_reasoning_roundtrip = true
```

## Final response reconstruction is lossy

For logging, one reference reconstructs a non-streaming final response from
stream chunks. It concatenates content, aggregates tool-call chunks by
index, keeps the last usage block, and even overrides `finish_reason` to
`tool_calls` when tool calls exist.

Design rule:

```text
Stream replay/summary is a derived artifact.
```

Mark reconstructed final responses with provenance:

```text
stream_reconstructed = true
source_chunks = n
finish_reason_source = provider | inferred | overridden
```

Never use a reconstructed response as if it were provider-native truth
unless the adapter proves equivalence for that protocol.

## Provider transforms can be behavior-preserving drops

The refs include request transforms like removing `tool_choice=auto` for a
provider that rejects it, because `auto` is already the default behavior.
That is not the same as silently dropping a meaningful user option.

Design rule:

```text
Some drops are semantic no-ops, but they still need provenance.
```

Classify transform outcomes:

```text
DroppedDefaultEquivalent
DroppedUnsupportedMeaningful
RewrittenEquivalent
RewrittenApproximate
RejectedUnsupported
```

This is especially important for `tool_choice`, thinking knobs, safety
settings, and provider-specific generation config.

## Safety/default injection can backfire

One reference removed automatic safety-settings merging because models that
did not support those categories returned 400s. Default injection is a
compatibility hack, not a harmless improvement.

Design rule:

```text
Do not inject provider defaults unless the model capability says they are accepted.
```

Keep defaults layered:

```text
client_defaults
proxy_policy_defaults
provider_required_defaults
model_safe_defaults
```

Each layer should be visible in translation warnings or trace metadata.

## Hidden response headers carry proxy truth

LiteLLM stores retry/fallback counts and provider cost values in hidden
response params, later rendered as extra response headers. That is a
side-channel, but it is useful for debugging routing and spend.

Design rule:

```text
Proxy operational metadata should not be mixed into provider JSON bodies.
```

Recommended side channel:

```text
CoreResponseMeta {
  attempted_retries,
  attempted_fallbacks,
  provider_cost,
  selected_provider,
  selected_credential,
}
```

Render this into protocol-appropriate headers or logs, not normalized
assistant content.

## Subrequests must inherit auth, budget, region, and user scopes

Context compaction and other internal subrequests are easy to undercount.
LiteLLM explicitly propagates parent metadata so summary calls debit the
same key/team/user/project budgets, obey model allowlists, preserve region
restrictions, and carry the end-user ID for older limiter hooks.

Design rule:

```text
Internal LLM calls are billable/rate-limited child requests.
```

For every subrequest, require:

```text
parent_request_id
budget_scope
rate_limit_scope
model_allowlist_scope
region_scope
end_user_scope
trace_span_parent
```

This applies to context compaction, web/search/tool helpers, advisor calls,
file-search emulation, and any future “agentic” helper call.

## Cache-control placement is provider-specific

OpenRouter supports cache-control for only certain model families and wants
`cache_control` inside content blocks. It also limits placement by adding it
only to the last block in a message to avoid breakpoint limits.

Design rule:

```text
Cache-control is structured prompt metadata, not a generic message flag.
```

Capabilities need more than a boolean:

```text
cache_control_supported
cache_control_location = message | content_block | provider_header
cache_control_max_breakpoints
cache_control_supported_content_types
```

## Local/open-compatible URL construction is a quirk

Ollama accepts OpenAI-like inputs but its real chat endpoint is `/api/chat`.
The adapter also maps OpenAI params to local names (`max_tokens` to
`num_predict`, JSON schema to `format`) and removes params that can make
requests hang (`tool_choice`, legacy `functions`).

Design rule:

```text
OpenAI-compatible does not mean URL-compatible or parameter-compatible.
```

For each provider config, split:

```text
base_url
endpoint_path
auth_style
param_map
dangerous_param_drops
```

Do not build upstream URLs by naive string concatenation without an
endpoint policy.
