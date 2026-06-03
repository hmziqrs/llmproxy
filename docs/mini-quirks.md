# Mini Quirks

Findings from a deeper pass over the reference projects. These are the
practical hacks mature proxies use because provider protocols do not
line up cleanly.

The main lesson:

```text
CoreChatStream is not passthrough.
CoreChatStream is a state machine.
```

## 1. Reasoning and thinking

Reasoning is stateful across turns.

Observed hacks:

- DeepSeek can require prior assistant `reasoning_content` to be sent
  back on later turns.
- Some proxies inject a single-space reasoning placeholder when the
  provider rejects an empty reasoning field.
- Some providers default to thinking mode unless explicitly disabled.
- Thinking can appear as a dedicated block or inline on a tool call.
- Anthropic thinking signatures and Gemini `thoughtSignature` are
  opaque but need to round-trip.

Design rule:

```text
Reasoning must be explicit CoreChat state.
```

Do not infer it late from raw provider JSON.

Core should preserve:

```text
thinking text
thinking signatures
redacted thinking
provider reasoning ids/signatures
whether reasoning was requested, disabled, or provider-defaulted
```

## 2. Streaming finalization

Providers disagree on when a stream is actually done.

Observed hacks:

- `finish_reason` may arrive before `usage`.
- `usage` may arrive in a separate final chunk.
- some streams end without usage.
- some streams end without an explicit finish event.
- some providers emit metadata-only chunks.
- some proxies synthesize final chunks so clients do not hang.
- some proxies emit `[DONE]` even after an error event.

Design rule:

```text
Usage-only, finish-only, and metadata-only chunks are real CoreChat events.
```

Core stream needs explicit terminal handling:

```text
MessageStart
ContentStart
Delta
Usage
MessageStop
Error
TransportClosed
```

Do not treat “no text delta” as “empty chunk.”

## 3. Tool calls

Tool calls are the messiest streaming case.

Observed hacks:

- tool call name/id may arrive before arguments.
- arguments may arrive as fragmented JSON.
- parallel tool calls are indexed differently by provider.
- some chunks contain empty/ghost tool calls.
- some providers omit IDs and proxies synthesize them.
- final `finish_reason` may say `stop` even though tool calls happened.
- some providers corrupt streams unless a dummy tool exists.
- some providers need tool names sanitized or prefixed.

Design rule:

```text
Tool calls need a dedicated stream accumulator.
```

Core should track:

```text
provider index
core tool_call_id
tool name
arguments delta
arguments buffer
started/stopped state
raw provider fragments
```

Final stop reason should be derived from normalized state, not blindly
trusted from provider text.

## 4. Usage accounting

Token accounting is not portable.

Observed hacks:

- OpenAI prompt tokens may include cached tokens.
- Anthropic input tokens exclude cache reads/writes.
- Gemini reports `promptTokenCount`, `cachedContentTokenCount`,
  `candidatesTokenCount`, and sometimes thinking tokens separately.
- reasoning tokens may be included in output tokens or separate.
- proxies often clamp negative values after cache subtraction.
- stream usage may only be visible in the final chunk.

Design rule:

```text
Usage must be rich and normalized once.
```

Core usage should include:

```rust
struct Usage {
    input_uncached: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}
```

Provider adapters can report raw usage in `provider_meta`, but the
core usage fields should have stable semantics.

## 5. Finish reasons

Finish reasons are not stable.

Observed hacks:

- `tool_calls`, `tool_use`, and `function_call` often mean the same
  thing.
- Gemini `MAX_TOKENS` maps to `max_tokens`.
- unknown finish reasons are often defaulted to `stop`.
- JSON mode may force `stop` even when the provider said something
  else.
- providers may emit `stop` even when tool calls were produced.

Design rule:

```text
Finish-reason mapping must be centralized.
```

Do not let every adapter invent its own mapping.

Core stop reasons should be a small enum:

```text
EndTurn
StopSequence
MaxTokens
ToolUse
ContentFilter
Safety
Error
Unknown
```

Keep the raw provider finish reason in metadata.

## 6. Provider metadata

Some provider data must survive even if core does not understand it.

Observed hacks:

- LiteLLM carries `provider_specific_fields`.
- MCP metadata can travel through stream chunks.
- Responses API may need encoded response/container IDs.
- Gemini needs thought signatures.
- Anthropic needs beta headers and context-management flags.
- provider-specific code interpreter items may replace generic
  function calls.

Design rule:

```text
Every normalized request, response, and event needs provider_meta.
```

This is the pressure-release valve. Use it deliberately.

## 7. Routing affinity

Follow-up requests may need to return to the same provider/model.

Observed hacks:

- LiteLLM encodes provider/model affinity into response IDs.
- container IDs and previous response IDs may carry routing data.
- encrypted content may be wrapped with model IDs.

Design rule:

```text
Plan for reversible IDs.
```

Not needed for the first simple chat path, but required for Responses
API, previous-response flows, containers, and multi-provider routing.

## 8. Retry after stream output

Retries are not equally safe.

Observed hacks:

- before visible output, retry/fallback is usually fine.
- after visible output, retry can duplicate text or corrupt tool state.
- some proxies only retry after output if the last chunk was
  reasoning-only and an explicit flag is enabled.

Design rule:

```text
Retry policy must know whether user-visible output escaped.
```

Core stream state should track:

```text
has_visible_output
has_tool_output
has_reasoning_only_output
```

## 9. Request sanitization

Providers reject fields that other providers accept.

Observed hacks:

- strip unsupported `cache_control`.
- strip or rewrite `dimensions`.
- move `reasoning_effort` into provider-specific `extra_body`.
- rewrite system messages into user/developer messages.
- delete `None` fields before validation.
- inject default schemas for empty tool definitions.
- rewrite `response_format` into tools for providers without native
  JSON mode.

Design rule:

```text
Adapters may sanitize, but core should record what was dropped or rewritten.
```

Use warnings:

```text
TranslationWarning::DroppedField
TranslationWarning::RewrittenField
TranslationWarning::SynthesizedField
```

## 10. Concrete Rust design implications

Core should have:

```text
CoreChatRequest
CoreChatResponse
CoreChatEvent
CoreContent
CoreToolCallState
Usage
StopReason
ReasoningState
ProviderMeta
TranslationWarning
StreamLifecycle
StreamAccumulator
RequestIdentity
CorePolicyTrace
CoreResponseMeta
HeaderMaps
StructuredOutput
ChildRequestScope
EndpointFamily
BatchPolicy
ErrorClassification
FileTransferState
CitationMeta
TokenCountResult
CostEstimate
ProviderRoute
BridgeRequest
BridgeResponse
PassThroughRouteState
LoggingPayload
PromptTemplateState
RoleAlternation
ContinuationPlaceholder
MultimodalBlockMap
```

Provider adapters should have:

```text
request sanitizer
response parser
stream parser
stream accumulator
usage mapper
finish-reason mapper
empty-chunk filter
error mapper
header renderer
structured-output mapper
policy transform tracer
endpoint router
batch result assembler
file/audio multipart builder
error classifier
route canonicalizer
bridge transformer
pass-through logger
prompt template normalizer
quirk flags/capabilities
```

Model config should use capabilities instead of scattered string
checks:

```toml
[models.deepseek]
requires_reasoning_roundtrip = true
defaults_to_reasoning = true
supports_cache_control = false

[models.gemini]
requires_thought_signature = true
supports_parallel_tool_calls = true

[models.qwen]
requires_dummy_tool_for_streaming = true
```

## 11. Deep audit additions

The second audit pass found several stricter requirements.

### Do not hide lossy paths

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

### Never invent usage

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

### No replay after visible output

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

### Terminal state is not just stop

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

### Null error codes are coerced so validation can pass

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

### Null `top_logprobs` are normalized to an empty list

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

### Usage token nulls are zero-filled before validation

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

### Null roles default to `assistant`

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

### Routing affinity is real

LiteLLM is the only reference with serious `previous_response_id`,
container, encrypted-content, and deployment affinity handling.

Design rule:

```text
Treat response IDs and container IDs as routing metadata, not just strings.
```

For v1 this can be deferred. For Responses API, it cannot.

### Container handlers must preserve URL query strings

The generic container endpoint handler appends path parameters to an API base
that may already carry a query string. It also avoids passing `params={}` to
`httpx`, because an empty params dict can strip the URL's own query string.
The handler uses `None` instead so existing query parameters survive.

Azure container URLs add one more twist: when the deployment's `api_base`
points at an Azure responses endpoint, the adapter strips that endpoint suffix
back to the resource root and prefers the `api-version` embedded in the base
URL over the deployment's own version field.

Design rule:

```text
Query preservation is part of URL construction, not request decoration.
```

Track:

```text
container_url_query_preserved
container_empty_params_as_none
container_path_append_with_query
azure_container_api_version_from_base
azure_container_resource_root_normalization
```

### Auth compatibility is a surface

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

### Beta headers and feature flags

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
aliases are resolved before filtering. That means the proxy is consulting a
capability registry, not forwarding raw headers.

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

### Cancellation and cleanup

Several references explicitly handle client disconnects during streaming.
The upstream request must be canceled and response bodies/stream wrappers
must be closed. Otherwise the proxy can leak sockets, keep paid upstream
requests running, or leave credential/session state dirty.

Design rule:

```text
Client disconnect is a terminal lifecycle event, not just a transport error.
```

Core stream state should distinguish:

```text
model_stop
provider_error
client_canceled
transport_closed
proxy_timeout
```

For Rust/Axum, make cancellation part of the stream executor contract:

```text
drop stream -> abort upstream task -> close body -> mark terminal state
```

### Credential and quota routing

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

### Unsupported parameter policy

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

### Message repair and sanitizer paths

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

### Context management cannot be assumed

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

### Router and fallback must share one chain

One reference has router fallback logic and circuit-breaker fallback
logic that can diverge.

Design rule:

```text
The router produces one ordered execution plan.
The executor consumes that exact plan.
```

Do not let the executor recompute fallback order.

### Capability registry beats provider name checks

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

## 12. Third audit additions

This pass looked for runtime hacks outside the obvious protocol
translation files.

### Duplicate request suppression

`oc-go-cc` hashes the raw request body and suppresses duplicate in-flight
requests inside a short window. This protects upstreams from client
double-submit bursts, but it is dangerous if treated as a generic cache.

Design rule:

```text
Deduplication is transport protection, not semantic response caching.
```

Track separately:

```text
request_identity_hash
dedup_window
idempotency_key
duplicate_policy
```

Do not deduplicate blindly across credentials, auth scopes, routing
targets, or requests with time-sensitive provider state.

### Stream errors after partial output

One proxy catches exceptions during SSE streaming and emits an error frame
plus `[DONE]` so the client is not left hanging. That is useful, but after
partial output the HTTP status cannot be changed and the stream may already
contain valid assistant text/tool deltas.

Design rule:

```text
Mid-stream errors are stream events, not normal HTTP errors.
```

Core needs:

```text
StreamError { visibility: BeforeFirstByte | AfterVisibleOutput }
```

Adapters should decide whether a target protocol can represent the error:

- OpenAI Chat SSE can emit an error-shaped `data:` frame, then `[DONE]`.
- Anthropic SSE can emit an `error` event.
- Some clients only tolerate transport close, so the proxy must log the
  terminal state even if it cannot send a clean protocol error.

### Final response reconstruction is lossy

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

### Cached stream replay can double count

LiteLLM defers success callbacks on streaming cache hits because cached
stream replay runs its own completion callbacks when the replay finishes.
Without this, spend/callback logs can double count the same logical request.

Design rule:

```text
Cache hit, stream replay, and success callback are separate lifecycle phases.
```

For the Rust proxy:

```text
CacheHit -> ReplayStart -> ReplayChunk* -> ReplayStop -> LogOnce
```

Usage/cost hooks should run exactly once per logical request.

### Empty chunk filtering is protocol-specific

LiteLLM has a deep `is_model_response_stream_empty` helper and Gemini
adapters skip chunks with no choices, no parts, or empty tool-call deltas.
But whitespace, usage-only chunks, finish-reason-only chunks, and
provider-specific extra fields can be meaningful.

Design rule:

```text
Empty is a protocol decision, not a generic truthy check.
```

Core should classify stream chunks:

```text
DataChunk
UsageOnlyChunk
FinishOnlyChunk
Heartbeat
NoiseChunk
ProviderMetaChunk
```

This avoids dropping late usage, finish reasons, pings, or provider
metadata just because no text was present.

### Partial tool-call JSON must be stateful

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

### Provider transforms can be behavior-preserving drops

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

### Safety/default injection can backfire

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

## 13. Fourth audit additions

This pass focused on policy, metadata, provider headers, and local/provider
URL behavior.

### Determinism knobs can hurt tool use

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

### Usage metadata sometimes must be requested

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

### OpenRouter packs route knobs into `extra_body`

OpenRouter keeps its own routing controls off the normal OpenAI param surface.
`transforms`, `models`, and `route` are popped out of the generic request and
reinserted under `extra_body` so the OpenAI client can still send them.

Design rule:

```text
Provider-private routing knobs should survive as extra_body, not leak into the normalized core.
```

Track:

```text
openrouter_extra_body_route
openrouter_extra_body_models
openrouter_extra_body_transforms
```

### Hidden response headers carry proxy truth

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

### Subrequests must inherit auth, budget, region, and user scopes

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

### Anthropic-only extras must not leak after translation

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

### Anthropic tool names are rewritten per request

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

### Anthropic history repair strips invalid thinking and empty text

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

### Cache-control placement is provider-specific

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

### Local/open-compatible URL construction is a quirk

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

### Transport selection can force IPv4

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

### Structured output is not one format

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

### Anthropic JSON mode strips internal response-format tool calls

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

### OCI tool schemas are rewritten before dispatch

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

### Gemini response schema keys are capability-swapped

Gemini request config does not keep a single schema key. If the model supports
`response_json_schema`, the transformer moves `response_schema` into the JSON
schema slot and drops the alternate key. Otherwise it rewrites the schema into
Vertex form and adds property ordering before dispatch.

Design rule:

```text
Schema-key selection can be model-capability dependent, not just renamed.
```

Track:

```text
gemini_response_schema_key_swap
gemini_response_json_schema_capability
gemini_vertex_schema_builder
gemini_property_ordering
```

### SAP Anthropic JSON may arrive wrapped in markdown

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

### Mistral strips schema noise and tool-message extras

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

### Vertex AI Claude strips unsupported `output_config` keys

Vertex AI Claude routes share a leaf sanitizer that mutates `output_config`
in place before dispatch. Non-dict values are dropped entirely. Unsupported
keys are filtered out. If the remaining dict is empty, the field is removed
instead of being sent as `{}`.

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

### Provider-specific headers are policy-scoped

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

## 14. Fifth audit additions

This pass focused on non-chat endpoint behavior that still affects a local
LLM proxy.

### Endpoint families need separate cores

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

### Embedding batching changes response assembly

One proxy optionally splits a multi-input embedding request into individual
server-side batcher calls, then reassembles the response by rewriting each
embedding item index and summing usage.

Design rule:

```text
Batching is an execution strategy that changes response reconstruction.
```

Track:

```text
original_input_order
per_item_request_id
per_item_usage
aggregate_usage
batch_reconstructed = true | false
```

For embeddings, preserving `data[*].index` is part of protocol
correctness, not just presentation.

### Embedding dimensions are not portable

Refs map OpenAI `dimensions` to provider-specific names like
`output_dimension`, while another request sanitizer drops `dimensions`
unless the model is known to support it.

Design rule:

```text
Embedding vector shape is a capability, not a generic optional param.
```

Capability fields:

```text
supports_embedding_dimensions
dimension_param_name
allowed_dimensions
default_dimension
embedding_modality
```

If dimensions are dropped, the response vector size may differ from what
the client expected. That requires a warning or rejection.

### Token-count endpoints are provider APIs, not local estimates

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

### Scenario routing is heuristic and request-shape dependent

`oc-go-cc` routes by token count, thinking keywords, tool-ish keywords, and
background-task patterns. For streaming, it intentionally prioritizes fast
TTFT over the most capable model.

Design rule:

```text
Routing heuristics are policy decisions with explainable reasons.
```

Store:

```text
route_scenario
route_reason
token_threshold_used
streaming_latency_bias
capability_downgrade = true | false
```

This helps debug why a complex streaming request went to a faster but less
capable model.

### Retry-after parsing is provider archaeology

The refs parse retry timing from standard headers, `x-ratelimit-reset`,
Google RPC `RetryInfo`, `ErrorInfo.quotaResetDelay`, free-text messages like
“quota will reset after 156h14m36s”, and millisecond strings. They also
distinguish transient rate limits from exhausted quota.

Design rule:

```text
Rate-limit metadata is structured after parsing, even when providers return text.
```

Core error fields:

```text
error_class = rate_limit | quota_exceeded | server_error | auth | forbidden | invalid_request | context_window
retry_after_seconds
quota_reset_timestamp
quota_id
quota_value
credential_rotatable
client_reportable
```

Do not treat all 429s the same.

### Client error reporting should hide normal churn

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

### File upload protocols can be multi-step

Gemini file upload uses resumable upload headers and a two-step flow:
metadata start request, then upload/finalize. Anthropic files require a beta
header, multipart form data, and a provider-specific default purpose.

Gemini file retrieval also normalizes file IDs before routing. It accepts raw
IDs, `files/<id>` forms, and full Google file URLs, then rewrites them into a
canonical `files/{encoded_id}` path before dispatch.

Mistral OCR looks like an upload flow but is still JSON-only: the adapter
keeps the document payload as a structured JSON object, filters OCR-specific
params against an allowlist, and never sets multipart form data.

Gemini model discovery also strips the `models/` prefix from the provider's
model list before re-adding LiteLLM's `gemini/` namespace. That keeps catalog
results aligned with the route format the rest of the proxy expects.

Design rule:

```text
Provider catalog entries need the same namespace normalization as live request routes.
```

Track:

```text
gemini_models_prefix_strip
gemini_model_list_namespace_normalization
gemini_catalog_route_alignment
Mistral_ocr_json_only
Mistral_ocr_no_multipart
Mistral_ocr_allowlisted_params
```

Azure image edit has a deployment-specific multipart rule: when the request
URL is an Azure `/openai/deployments/{deployment}/images/edits` route, the
adapter strips `model` out of the multipart form payload because the
deployment is already encoded in the URL.

Azure image generation follows the same deployment-first rule for JSON
bodies: when the request is routed through `/openai/deployments/{deployment}/images/generations`,
the adapter removes `model` from the JSON payload so the deployment name is
only expressed in the URL.

Design rule:

```text
Multipart payloads must match whether identity lives in the URL or the form body.
```

Track:

```text
azure_image_edit_strip_model_from_multipart
azure_image_edit_deployment_url_identity
azure_image_edit_finalize_form_data
azure_image_generation_strip_model_from_json
azure_image_generation_deployment_url_identity
azure_image_generation_body_cleanup
```

### Gemini realtime remaps GA session fields back to beta keys

Gemini Live session updates can arrive in the GA nested shape. The realtime
adapter lifts `output_modalities`, `audio.input.transcription`, and
`audio.input.turn_detection` back into the flat beta keys that the existing
mapper understands, then deep-merges follow-up setups so partial updates do
not discard earlier session config.

That merge is itself nested: `automaticActivityDetection` is merged by key so
partial VAD updates do not blow away earlier knobs like
`silenceDurationMs` or `prefixPaddingMs`.

Design rule:

```text
Realtime bridging should normalize GA shapes before existing mapper logic runs.
```

Track:

```text
gemini_realtime_ga_field_remap
gemini_realtime_session_merge
gemini_realtime_turn_detection_lift
gemini_realtime_automatic_activity_detection_merge
```

Design rule:

```text
File upload is a state machine, not a single POST body.
```

Core file state:

```text
UploadStart
UploadBytes
UploadFinalize
UploadComplete
FileMetadata
FileContentDownload
DeleteFile
```

The adapter owns provider-specific upload headers, content type, filename
defaults, purpose mapping, and ID/path encoding.

### Gemini file search is a generateContent bridge

Gemini file search does not have a dedicated search endpoint. The vector-store
adapter turns search requests into `generateContent` calls with a `file_search`
tool, converts filter syntax into Gemini's metadata filter string, and then
reconstructs search results from `groundingMetadata` and `retrievedContext`
chunks in the response.

Design rule:

```text
File search can be a tool-backed generation flow, not a separate search API.
```

Track:

```text
gemini_file_search_generate_content
gemini_file_search_metadata_filter
gemini_file_search_grounding_metadata
gemini_file_search_retrieved_context
```

### Multipart endpoints need header surgery

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

### Mistral transcription preserves extra fields in hidden params

Mistral Voxtral audio transcription returns more than just the transcript
text. LiteLLM lifts `text` into the public transcription response, but keeps
Mistral-specific `segments` and `language` fields in `_hidden_params` so
callers can still recover the richer provider payload later.

Design rule:

```text
Transcription adapters may expose a simplified public response while preserving provider extras.
```

Track:

```text
mistral_transcription_hidden_segments
mistral_transcription_hidden_language
mistral_transcription_public_text
```

### Azure Speech STT rewrites the base URL and response format

Azure AI Speech transcription only accepts Cognitive Services or STT Speech
endpoints. The adapter rejects Azure OpenAI endpoints, normalizes the base URL
to the STT host, and maps OpenAI `response_format=verbose_json` to Azure's
`format=detailed` query value while building the request URL.

Design rule:

```text
Speech-to-text routing must validate the endpoint family and translate response-format names.
```

Track:

```text
azure_speech_stt_base_url_resolution
azure_speech_stt_reject_azure_openai_endpoint
azure_speech_stt_verbose_json_to_detailed
```

### OpenAI Whisper forces verbose_json for duration-aware cost tracking

OpenAI transcription requests upgrade `response_format` to `verbose_json`
when the caller asked for plain text or JSON. That ensures the upstream
response includes duration metadata, which the proxy uses for cost
calculation and downstream accounting.

Design rule:

```text
If the proxy needs duration metadata, transcription response format is part of cost policy.
```

Track:

```text
openai_whisper_force_verbose_json
openai_whisper_duration_cost_tracking
openai_whisper_response_format_upgrade
```

### Image generation has model-dependent endpoints

Gemini image generation uses `:generateContent` for Gemini Flash image
preview models but `:predict` for Imagen models. OpenAI `size` maps to
provider aspect ratios, and usage can contain modality-specific token
details.

OpenAI image-edit routing also normalizes the model name before choosing a
config class: `dall-e-2`, `dall_e_2`, and `dalle2` all collapse to the same
branch, while everything else uses the default image-edit config.

OpenAI image variations also have a hidden default: when no model is passed,
the handler falls back to `dall-e-2` before resolving the provider config.

OpenRouter image generation keeps the OpenAI surface but reshapes `size` and
`quality` into provider-native `image_config` fields: `size` becomes
`aspect_ratio`, `quality` becomes `image_size`, and the generated image is
returned inside chat-completion message content rather than a separate image
endpoint response.

OpenRouter image edit uses the same chat-completions bridge: the source image
is inlined as a base64 `data:` URL inside a single user message, the text
prompt is appended as another content part, `modalities` is forced to
`["image", "text"]`, and the request stays JSON-only rather than multipart.

Bedrock Nova Canvas image edit ignores `response_format` values other than
`b64_json` and always returns base64 images. It also rewrites OpenAI-style
`size`, `n`, `quality`, and nested `imageGenerationConfig` into Bedrock's
width/height/numberOfImages/config shape before dispatch.

Bedrock Nova Canvas generation also strips the user-supplied `model_id`
before building the request object, because that identifier is only there to
avoid Bedrock's extraneous-key errors and is not part of the upstream schema.

Bedrock Nova Canvas request construction is task-driven: `taskType`
determines whether the adapter builds text-to-image, color-guided generation,
or inpainting payloads, and the default task is `TEXT_IMAGE` when the caller
does not specify one.

Bedrock Nova Canvas model support is also catalog-driven: the adapter resolves
support from `model_cost` metadata and tries stripped aliases and
cross-region/base-model variants, rather than trusting the raw model string
alone.

Bedrock Stability image edit is stricter than the other image adapters:
`size` is converted to Stability's `aspect_ratio`, `n` is rewritten to
`_n` for internal handling, `response_format` is treated as a postprocessing
hint only, and unsupported keys raise unless `drop_params` is enabled.

Gemini image edit is a JSON-only inline-image flow: it refuses multipart,
requires at least one image, base64-encodes `inlineData` parts, appends the
prompt as a text part when present, and moves OpenAI `size` into
`generationConfig.imageConfig.aspectRatio`.

Llamafile fakes an API key when none is provided, because the underlying
OpenAI client still wants one even though the Llamafile server does not
require bearer auth.

Featherless AI only accepts `tool_choice=auto|none`. Any other `tool_choice`
value or any `tools` payload is treated as unsupported and either dropped
when `drop_params` is enabled or rejected with an error.

Replicate splits model identity into request routing and payload metadata: a
colon-delimited model string can become the `version` field, deployment IDs
switch the request path to `/v1/deployments/.../predictions`, and when the
model says it supports system prompts the first system message is lifted out
into a separate `system_prompt` field.

Bedrock IAM credential handling uses a process-wide in-memory cache with
different TTL behavior depending on auth source: static access-key
credentials are cached longer, ambient environment credentials use the cache
default TTL, and AssumeRole, web identity, profiles, and session-token tuples
are intentionally not cached so refresh state does not bleed across logical
sessions.

OpenAI audio-transcription guardrails are output-only: the input path is a
no-op because the payload is binary audio, while the transcribed text is
re-guardrailed after transcription. The handler also injects request metadata
into `request_data` so the guardrail layer can see the response context.

Design rule:

```text
Image generation endpoint shape depends on model family.
```

Capabilities:

```text
image_endpoint_kind = generate_content | predict | provider_native
size_mapping = pixels | aspect_ratio | unsupported
image_usage_details = text_tokens | image_tokens | output_tokens
response_modalities
image_edit_model_normalization
openai_image_variation_default_model
openrouter_image_config_aspect_ratio
openrouter_image_config_image_size
openrouter_image_in_message_content
openrouter_image_edit_chat_bridge
openrouter_image_edit_data_url
openrouter_image_edit_modalities
bedrock_stability_aspect_ratio_mapping
bedrock_stability_response_format_hint
bedrock_stability_drop_params_gate
gemini_image_edit_inline_data
gemini_image_edit_json_only
gemini_image_edit_aspect_ratio_mapping
bedrock_nova_canvas_task_type_dispatch
llamafile_fake_api_key
featherless_tool_choice_gate
replicate_version_request_split
bedrock_iam_credential_cache_ttl
bedrock_nova_canvas_model_cost_resolution
openai_audio_transcription_output_only_guardrails
```

### OpenRouter Responses stays HTTP, not native WebSocket

OpenRouter's Responses API is exposed as a normal HTTP endpoint and explicitly
does not advertise native WebSocket support. That means the transport layer
must treat it as a request/response route, not as a realtime socket family.

Design rule:

```text
Do not infer websocket capability from a Responses endpoint just because it is provider-native.
```

Track:

```text
openrouter_responses_http_only
openrouter_responses_no_native_websocket
openrouter_responses_transport_family
```

### Gemini video generation is long-running and size-aware

Gemini Veo video generation is not a one-shot response. It starts with
`predictLongRunning`, then polls an operation until completion, then fetches
the generated video via the file API. The adapter also maps OpenAI-style
`size` values into Gemini `aspectRatio` and, when possible, `resolution`,
while defaulting `seconds` to 4 if the caller does not provide one.

Design rule:

```text
Long-running media generation needs its own lifecycle and parameter mapping.
```

Track:

```text
gemini_video_predict_long_running
gemini_video_operation_polling
gemini_video_size_to_aspect_ratio
gemini_video_size_to_resolution
gemini_video_default_seconds
```

### Azure TTS normalizes voice, format, and speed into SSML

Azure AVA text-to-speech does not forward OpenAI TTS fields directly. It maps
OpenAI voice aliases to Azure voice names, maps response formats to Azure
output-format strings, converts `speed` into an SSML prosody rate, and treats
any input that already looks like SSML as a pass-through body instead of
wrapping it again.

Design rule:

```text
Speech synthesis needs SSML-aware normalization, not chat-style parameter passthrough.
```

Track:

```text
azure_tts_voice_alias
azure_tts_output_format_map
azure_tts_speed_to_rate
azure_tts_ssml_passthrough
```

### Citations and search results are response side channels

Perplexity-style responses attach citations/search results outside normal
assistant text, and then derive OpenAI annotations by matching `[1]` markers
inside the message content.

Design rule:

```text
Citations are structured metadata, not just text post-processing.
```

Core response should preserve:

```text
raw_citations
search_results
text_annotations
annotation_source = provider | inferred_from_text
```

When translating to a protocol without annotations, emit a warning instead
of flattening citations invisibly.

### Cost estimate endpoints are estimates with source labels

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

## 15. Sixth audit additions

This pass focused on route canonicalization and protocol bridges.

### Provider routes are canonical identity

The provider catalogs use route prefixes as the canonical identifier when
available, and fall back to API-key prefixes only when a provider has no
route. Custom OpenAI-compatible providers are also remapped to `openai/`
with `custom_llm_provider="openai"` so they can ride the same execution path.

Design rule:

```text
Provider identity should be route-first, not label-first.
```

Track:

```text
provider_route
canonical_provider_key
custom_provider_override
api_base_override
route_source = catalog | env | custom_openai_compat
```

This matters for config, logging, cost, auth, and capability lookup.

### Interactions and Responses are bridge protocols

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

### Gemini Interactions folds MIME and image config into response_format

The Gemini Interactions adapter treats `response_mime_type` and
`generation_config.image_config` as schema inputs, not free-floating request
fields. On the new API revision it folds `response_mime_type` into
`response_format`, strips the source field, and lifts `image_config` out of
`generation_config` into a `response_format` entry with `type="image"`.

Design rule:

```text
Provider revisions can move response-shape fields across request sections.
```

Track:

```text
gemini_interactions_response_mime_type_fold
gemini_interactions_image_config_lift
gemini_interactions_legacy_schema_passthrough
```

### Pass-through logging is route-specific

LiteLLM pass-through logging chooses a handler based on route family
(`Anthropic`, `Vertex`, `OpenAI`, `Gemini`, etc.), reconstructs a standard
logging object from raw SSE bytes, and schedules success logging even after
client disconnect so partial usage can still be tracked. The logging path
also extracts model names from request body, logging state, or route URL.

Design rule:

```text
Pass-through transports need route-family parsers, not one generic parser.
```

Track:

```text
route_family
raw_sse_bytes
parsed_standard_log
cost_injection_active
disconnect_safe_logging
model_source = request | log_state | url
```

Do not let pass-through logging mutate the user-visible stream contract.

### Ollama strips provider prefixes before model lookup

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

### Passthrough cost is precomputed and injected into hidden params

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

## 16. Seventh audit additions

This pass focused on prompt-template normalization and multimodal block
conversion.

### Role alternation is a hidden prompt rewrite

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

### Prompt templates are model-family adapters

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

### Multimodal blocks need content-type routing

The template helpers route `image_url`, `input_audio`, `file`, and PDF/text
data URIs through different block types. Some providers want base64 image
strings, some want document blocks, and some need the image URL converted to
base64 first. Anthropic also needs tool-use IDs sanitized and server-side
tool blocks preserved separately.

Design rule:

```text
Multimodal block selection depends on media type and provider shape.
```

Track:

```text
content_type = text | image | document | audio | tool_use | server_tool_use
media_route = url | base64 | file_id | inline_data
tool_use_id_sanitized
server_tool_preserved
```

### Gemini MIME types are normalized before file routing

The Gemini multimodal transformer normalizes MIME strings before it decides
how to route a file or URL. It strips MIME parameters like `; charset=utf-8`,
then applies known aliases such as `image/jpg -> image/jpeg`. That keeps GCS
metadata and caller-supplied MIME hints on the same path.

Design rule:

```text
Media routing should normalize MIME aliases and parameters before validation.
```

Track:

```text
gemini_mime_alias_normalization
mime_parameter_stripping
gcs_content_type_normalization
```

### Bedrock strips Claude Code custom tool fields

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

### Claude Platform on AWS is a routed Bedrock variant

The Claude Platform path is not a plain Bedrock alias. It strips the
`claude_platform/` route prefix before handing the model to the Anthropic
transformer, requires a workspace ID, and injects `anthropic-workspace-id`
plus a fallback `x-api-key` when needed.

Design rule:

```text
Route prefixes, workspace identity, and upstream auth need to stay coupled for Claude Platform.
```

Track:

```text
claude_platform_route_strip
claude_platform_workspace_id
claude_platform_anthropic_workspace_header
claude_platform_fallback_x_api_key
```

### Bedrock Invoke Agent is its own route and trace protocol

Invoke Agent does not proxy the normal chat request surface. It builds an
`inputText` request from the last user message, forces `enableTrace=true`,
and encodes `agent_id`, `agent_alias_id`, and `session_id` into the URL path.
The response parser then reassembles chunk content from AWS event-stream
chunks and derives usage from the trace payload.

Design rule:

```text
Agent runtimes need route-encoded identity and trace-derived usage, not plain chat plumbing.
```

Track:

```text
bedrock_invoke_agent_url_identity
bedrock_invoke_agent_enable_trace
bedrock_invoke_agent_chunk_reassembly
bedrock_invoke_agent_trace_usage
```

### Azure Assistants backfills missing message status

Azure Assistants thread messages can come back without a `status` field.
The adapter treats that as an incomplete OpenAI object, patches the message
to `completed`, and then rehydrates it into the LiteLLM response shape.

Design rule:

```text
If the upstream object omits a lifecycle field, the adapter may need to synthesize it before returning.
```

Track:

```text
azure_assistants_default_message_status
azure_assistants_message_rehydration
azure_assistants_status_backfill
```

### Bedrock Invoke inlines structured output into the last user message

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

### Bedrock caps Claude Opus `output_config.effort` to the provider ceiling

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

### Bedrock Converse rewrites guarded text for guardrails

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

### Azure Responses O-series drops unsupported temperature

Azure OpenAI O-series Responses models do not accept `temperature` the way
the base Responses API does. The adapter removes it only when `drop_params`
is enabled, and its supported-parameter list excludes it up front so the
request surface matches the model family.

Design rule:

```text
Family-specific responses endpoints need their own unsupported-param policy.
```

Track:

```text
azure_o_series_responses_temperature_drop
azure_o_series_responses_supported_params
azure_o_series_responses_drop_policy
```

### Azure Responses strips reasoning status fields

Azure OpenAI Responses rebuilds reasoning items before validation so the
provider sees a shape it accepts. That includes synthesizing a summary when
needed and removing `status` from reasoning items; the fallback path also
drops other None-heavy fields when object construction fails.

Design rule:

```text
Provider responses that reject a field need object-level repair, not just key filtering.
```

Track:

```text
azure_responses_reasoning_status_strip
azure_responses_reasoning_item_rebuild
azure_responses_reasoning_fallback_filter
```

### Azure chat gates `tool_choice` by API version

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

### Tool-call repair belongs in the prompt layer too

Prompt utilities attempt to repair truncated tool-call JSON, add warnings
when repair succeeds, and sanitize Anthropic `tool_use_id` values to match
the provider regex. That means some “prompt” bugs are really serialization
and repair bugs.

Design rule:

```text
Tool-call repair is part of prompt rendering, not just response parsing.
```

## 18. Eighth audit additions

### Background side effects need durability classes

The audited code splits side effects into different durability tiers:

- logging work is best-effort, bounded, and allowed to drop or retry later
- buffered writes are retryable and intended to survive process exit
- stateful writes keep an in-memory truth even when disk is unhealthy
- stream cleanup is attempted in `finally`, but client disconnect or shutdown can still cut it short

That means the proxy needs an explicit policy for each side effect instead of a single generic “background task” bucket.

Design rule:

```text
Every side effect must declare its durability class.
```

Track:

```text
durability = drop | retry | flush_on_exit | persist_until_success
side_effect_kind = log | usage_state | credential_state | cache_write | stream_cleanup
```

### Shutdown is a first-class state

The logging worker and buffered-write registry both treat process exit as a real lifecycle event:

- `atexit` hooks try to flush remaining work
- worker shutdown cancels in-flight tasks and then drains what it can
- bounded queues may clear aggressively or retry after cooldown
- retry threads stop, then a final flush is attempted

This is not a nice-to-have. If the proxy owns billing, usage, credentials, or audit logs, shutdown behavior affects correctness.

Design rule:

```text
Shutdown is part of the protocol, not an afterthought.
```

Track:

```text
shutdown_phase = running | draining | flushing | stopped
queue_policy = bounded | aggressive_clear | delayed_retry
pending_write_state = in_memory_only | registered | flushed
```

### Disk health and memory truth can diverge

`ResilientStateWriter` keeps current state in memory even when disk writes fail. It retries later, registers the write for shutdown flush, and only clears the pending state after success. So “written” and “durable” are separate facts.

Design rule:

```text
In-memory state is authoritative until durability succeeds.
```

Track:

```text
memory_state_version
disk_state_version
last_write_error
retry_due_at
```

### Anthropic experimental pass-through has its own context polyfills

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

### Anthropic experimental pass-through can reroute thinking to Responses

When the Anthropic pass-through is used for an OpenAI model and the caller
enables `thinking`, the adapter converts that into OpenAI reasoning params
and prefixes the model with `responses/` so the request goes through the
Responses API instead of Chat Completions. That is how it preserves a
thinking-like Anthropic surface on OpenAI models that would otherwise only
return token accounting.

Design rule:

```text
Protocol bridges may need to change both the request body and the target route.
```

Track:

```text
anthropic_pass_through_responses_route
anthropic_thinking_to_reasoning_effort
thinking_summary_passthrough
```

### cache_control is stripped and reshaped per provider family

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

## 19. Ninth audit additions

### Responses-style streams have event semantics, not chunk semantics

The Responses bridge does not treat every chunk as a terminal-bearing message.
It maps `response.created`, `response.output_item.added`, and
`response.function_call_arguments.delta` into OpenAI-style chunks with
`finish_reason = None`, then waits for `response.completed` to emit the final
terminal state.

That means the stream protocol is event-driven, not token-chunk-driven.
If you translate it like a plain chat stream, you end the stream too early and
lose later tool calls or reasoning items.

Design rule:

```text
Responses streams terminate only on the terminal event, not on the last visible delta.
```

Track:

```text
responses_event_type = created | output_item_added | function_call_delta | completed | failed
terminal_event_seen
intermediate_finish_reason = none
```

### Streaming tool calls are accumulated by index before they are emitted

Google GenAI streaming tool calls are not independent self-contained chunks.
The adapter accumulates name and arguments by `tool_call.index`, skips empty
chunks, and only emits a function call once the JSON arguments parse.

That means tool-call assembly is stateful per index, and the ordering of name
versus argument fragments matters.

Design rule:

```text
Tool-call streaming is an indexed accumulator, not a stateless delta mapper.
```

Track:

```text
tool_call_index
tool_call_name_buffer
tool_call_arguments_buffer
tool_call_parsed
tool_call_empty_chunk_skipped
```

### Google stream endpoints suppress OpenAI's `[DONE]` terminator

The Google `streamGenerateContent` proxy path sets an internal flag to stop
the OpenAI-style stream wrapper from appending its usual `[DONE]` terminator.
This is not a cosmetic tweak. The Google GenAI SSE client expects a different
end-of-stream contract, so the proxy has to suppress the OpenAI sentinel
entirely.

Design rule:

```text
Wire terminators are protocol-specific; do not inherit OpenAI stream endings by default.
```

Track:

```text
skip_openai_stream_done
google_sse_terminator
non_openai_stream_contract
```

### Google GenAI request bodies rename `generationConfig` to `config`

The Google GenAI route preprocessing step rewrites `generationConfig` into
`config` for `generateContent` and `streamGenerateContent` requests when the
caller did not already supply `config`. This is a compatibility shim, not a
no-op: the downstream request shape changes before routing.

Design rule:

```text
Normalize provider-specific request field names before dispatch.
```

Track:

```text
google_generation_config_alias
request_body_field_rename
pre_route_google_normalization
```

### Provider-specific fields must survive the bridge

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

## 20. Tenth audit additions

### DeepSeek thinking mode is history-driven and can be forcibly disabled

The Go proxy does more than forward a `thinking` field. It inspects the
entire assistant history to decide whether thinking mode is already active.
If a DeepSeek request has assistant history but no thinking blocks, it sends
`thinking: {"type":"disabled"}` to override DeepSeek's default thinking mode
and prevent the next turn from requiring `reasoning_content`.

It also treats inline `thinking` attached to a `tool_use` block as real
thinking history, not as an optional annotation. That means the proxy has to
detect multiple shapes of the same semantic state.

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

### Gemini system instructions are represented as synthetic turns

The Gemini transformer does not preserve Anthropic system instructions as a
native system slot. It emits a synthetic user turn containing the instruction
and then a synthetic model acknowledgement. That is a protocol bridge hack,
not a semantic no-op.

Design rule:

```text
If a target protocol has no native system slot, the system prompt becomes explicit conversation state.
```

Track:

```text
synthetic_system_turn
synthetic_ack_turn
system_instruction_transport
```

## 21. Eleventh audit additions

### Nested drop params are applied before provider transforms

The custom HTTPX handler does not wait until the provider request is fully
built before removing unsupported fields. It strips nested paths from
`anthropic_messages_optional_request_params` up front, before the provider
transform runs.

That makes `additional_drop_params` a pre-transform policy knob, not a generic
JSON cleanup after serialization.

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

### Some handlers preserve original request context for downstream hooks

The Responses API handler deliberately keeps the pre-transform request context
around so post-call hooks and metadata see the original params rather than the
provider-shaped body. That means hook semantics depend on the unmodified
request graph, not just the upstream payload.

Design rule:

```text
Post-call hooks should observe original request intent, not only provider wire format.
```

Track:

```text
original_request_context
provider_shaped_body
hook_visibility_scope
```

### Realtime beta headers gate the event contract

OpenAI realtime keeps the upstream `OpenAI-Beta: realtime=v1` header only if
the client sent it to the proxy. GA clients are forwarded without the beta
header and therefore need the GA-shaped session/update event vocabulary.

Design rule:

```text
Realtime protocol version is negotiated by header propagation, not by route name alone.
```

Track:

```text
realtime_beta_header_forwarding
realtime_ga_event_shape
realtime_protocol_negotiation
```

### Bedrock Nova Sonic realtime has fixed audio sample-rate defaults

Bedrock Nova Sonic realtime does not reuse OpenAI's audio defaults. The
adapter starts with 24kHz output audio and 16kHz input audio, then maps
OpenAI audio formats (`pcm16`, `g711_ulaw`, `g711_alaw`) onto those sample
rates when session updates arrive.

Design rule:

```text
Realtime audio adapters need provider-specific sample-rate defaults, not one shared PCM assumption.
```

Track:

```text
bedrock_nova_sonic_output_sample_rate_hz
bedrock_nova_sonic_input_sample_rate_hz
bedrock_nova_sonic_audio_format_mapping
```

### WebSocket responses need the model injected into the URL

The OpenAI responses websocket path requires `model` in the query string, and
the handler preserves pre-existing query parameters when adding it. That makes
the URL itself part of the protocol contract.

Design rule:

```text
When the transport uses URL parameters for identity, URL rewriting is part of normalization.
```

Track:

```text
websocket_model_param
preserve_existing_query_params
url_level_identity
```

## 22. Twelfth audit additions

### ChatGPT backend requests are force-shaped, not pass-through

The ChatGPT provider adapter does not simply forward Responses API knobs.
It forces `store = False`, forces `stream = True`, injects
`reasoning.encrypted_content` into `include`, and then drops every request key
outside a small allowlist.

That means the caller’s request is being translated into the provider’s
internal contract, not merely normalized.

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

### ChatGPT backend tool-call streams need index repair and duplicate suppression

The ChatGPT backend API emits non-spec tool-call chunks: all indices come back
as `0`, `id`/`name` get repeated in closing chunks, and the normalizer has to
assign stable indices while skipping duplicate closing chunks.

That makes the stream a repair job, not a direct decode.

Design rule:

```text
Backend-only stream shapes must be normalized before they enter the shared protocol layer.
```

Track:

```text
tool_call_index_repair
duplicate_closing_chunk_skip
last_tool_call_id
stable_tool_call_index
```

## 23. Fourteenth audit additions

### Polling mode is a Redis snapshot of the stream, not a copied final response

The background polling path does not wait for a normal response object and
then persist it once. It streams the provider response, incrementally updates a
Redis-backed `ResponsesAPIResponse`, and flushes partial state on a timer.

Terminal state is also event-driven here: `response.completed`, `failed`,
`incomplete`, and `cancelled` each map to different OpenAI status values, and
the final state is assembled from the stream plus the terminal event payload.

That means polling is not “store the final object later.” It is “continuously
rebuild the object while the stream is still live.”

Design rule:

```text
Polling state is a live snapshot, not a delayed copy of the final response.
```

Track:

```text
polling_id
redis_snapshot_state
terminal_status
terminal_error
state_flush_interval
```

## 24. Eighteenth audit additions

### Emulated file_search is a synthetic two-step response

When file_search is not natively supported, the Responses layer replaces it
with a function tool, runs vector search itself, and synthesizes an
OpenAI-shaped response with:

- a `file_search_call` output item
- a `message` output item with `file_citation` annotations
- optional `search_results` if the caller requested `file_search_call.results`

It also disables streaming for that path, because the emulation depends on
reassembling the response object before returning it.

Design rule:

```text
Emulated tools are their own protocol, not a thin compatibility flag.
```

Track:

```text
synthetic_file_search_call
file_citation_annotations
include_search_results
stream_disabled_for_emulation
```

## 25. Nineteenth audit additions

### MCP auto-execute rewrites a single request into a staged two-call flow

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

### MCP streaming emits synthetic discovery and tool-execution events

The MCP streaming iterator does not only relay model tokens. It injects its own
event phases around the model stream: MCP discovery events, tool-execution
events, and then the follow-up response. Discovery happens after the initial
`response.output_item.added` phase, not at the start of the stream.

Tool execution is also emitted as structured MCP stream events with stable
item IDs, sequence numbers, and synthetic `approval_request_id` values. That
means the stream is partly model output and partly proxy-generated control
traffic.

Design rule:

```text
MCP streaming is a phased event generator with proxy-owned control events, not a plain token pipe.
```

Track:

```text
mcp_discovery_phase
synthetic_tool_execution_events
approval_request_id_generation
phase_based_stream_switching
```

## 24. Seventeenth audit additions

### Failed or incomplete Response streams still materialize a completed response

The Responses streaming iterator stores `completed_response` for
`response.completed`, `response.incomplete`, and `response.failed`. That lets
cost annotation, logging, and failure hooks run even when the stream does not
end in a clean success.

So “completed response object exists” is not the same as “request succeeded.”
It is a hook-carrier object for the stream finalization path.

Design rule:

```text
Final response objects can exist for failed streams when side effects still need a carrier.
```

Track:

```text
completed_response_carrier
failed_stream_logging
incomplete_stream_logging
cost_annotation_on_failure
```

## 25. Sixteenth audit additions

### Streaming output items are re-identified for follow-up routing

The Responses stream iterator does not leave `container_id` and
`encrypted_content` untouched. It rewrites output-item IDs with model/provider
affinity and wraps encrypted content with the model ID when the affinity flag
is enabled, so UI and proxy follow-ups can route back to the right context.

That is a protocol identity mutation, not just a metadata tag.

Design rule:

```text
Streamed identities must be rewritten when downstream routing depends on them.
```

Track:

```text
container_id_wrapped
encrypted_content_wrapped
model_affinity_enabled
follow_up_routing_identity
```

## 26. Fifteenth audit additions

### Responses stream iterators use a priority queue of synthetic events

The `LiteLLMCompletionStreamingIterator` does not emit raw chat chunks in
arrival order. It maintains pending queues for response events, tool events,
and annotation events, then drains them with a strict priority order:

1. initial `response.created` / `response.in_progress`
2. pending response events
3. pending tool events
4. the current chunk transformed into a Responses event
5. annotation events when present

It also emits reasoning summary text/part/done events as a staged sequence
before `response.completed`. So the final Responses stream is a synthetic event
schedule, not a direct projection of the upstream chunk stream.

Design rule:

```text
Responses streams are staged event pipelines with explicit priority, not pass-through chunk logs.
```

Track:

```text
pending_response_events
pending_tool_events
pending_annotation_events
reasoning_summary_done_sequence
```

## 27. Thirteenth audit additions

### SSE recovery fabricates stable slots when the stream omits indices

The shared SSE recovery helpers do not assume the provider will always send
`output_index` or `content_index`. If `output_index` is missing, they fall back
to the next free slot. If `OUTPUT_TEXT_DONE` arrives without a matching output
item, they synthesize a message item so the recovered response still has a
coherent shape.

This also means there is a hard cap on how far a sparse `content_index` can
jump before the helper refuses the chunk. That is a safety boundary, not just
an implementation detail.

Design rule:

```text
Recovery code may synthesize structure, but it must bound the damage from malformed indices.
```

Track:

```text
output_index_fallback
content_index_fallback
synthetic_text_only_item
max_content_index
```

## 28. Twentieth audit additions

### Managed batch and fine-tune IDs are rewritten with model affinity

The batch and fine-tuning proxy endpoints do not treat IDs as opaque strings.
If an input file ID or batch ID is encoded with model information, the proxy
uses that encoding to route the request through the right credentials/model
path, then rewrites the returned ID for the caller while preserving the
encoded value in `_hidden_params`.

For batches, the proxy can also recover a model ID from the encoded batch ID
and stash it alongside `unified_batch_id` so later lookups keep the same model
affinity. For fine-tuning, managed training file IDs and fine-tuning job IDs
go through the same hidden metadata path.

Design rule:

```text
Managed async resource IDs are routing state, not opaque identifiers.
```

Track:

```text
unified_batch_id
unified_file_id
unified_finetuning_job_id
model_id_from_unified_id
hidden_id_affinity
```

### Bedrock batch polling resolves region from the ARN

Bedrock batch polling is not region-agnostic. The handler resolves region in
priority order: explicit region, region parsed from the batch ARN, then
`us-east-1` as the boto3 default. It also returns `request_counts = (0, 0, 0)`
because `GetModelInvocationJob` does not expose per-record counts.

Design rule:

```text
Batch polling needs region resolution and an explicit "counts unavailable" signal.
```

Track:

```text
bedrock_batch_region_resolution
bedrock_batch_counts_unavailable
bedrock_batch_arn_region_fallback
```

### Bedrock embeddings strip auth params and default the region

Bedrock embeddings mutate their optional params before dispatch: auth-related
kwargs are popped out so they do not leak into the model request, and the
region resolver falls back to `us-west-2` when neither explicit input nor
environment values provide one.

Design rule:

```text
Embedding transport should sanitize auth inputs and resolve region explicitly.
```

Track:

```text
bedrock_embedding_auth_param_strip
bedrock_embedding_region_default
bedrock_embedding_request_sanitization
```

### Vector store searches carry provider-specific query semantics

OpenAI vector-store search forwards `rewrite_query` as part of the request
body, so the proxy needs to preserve that field as a first-class search
control rather than collapsing it into the generic query string.

Bedrock Knowledge Base search rewrites OpenAI-style filters into AWS filter
trees. Single operators become direct operator nodes, while `and` / `or`
filters are converted into `andAll` / `orAll` structures. A single-item
`and` or `or` is unwrapped because AWS requires at least two elements for the
compound form.

Design rule:

```text
Vector-store filters and query rewriting are provider-native search semantics, not generic metadata.
```

Track:

```text
openai_vector_store_rewrite_query
bedrock_vector_store_filter_tree_mapping
bedrock_vector_store_single_filter_unwrap
```

### Bedrock rerank rewrites the runtime host to agent-runtime

Bedrock rerank does not call the normal Bedrock runtime host directly. The
handler takes the resolved runtime endpoint, rewrites `bedrock-runtime` to
`bedrock-agent-runtime`, and then appends `/rerank` before signing and
sending the request.

Design rule:

```text
Rerank is a different Bedrock host family, not just another path.
```

Track:

```text
bedrock_rerank_agent_runtime_host
bedrock_rerank_endpoint_rewrite
bedrock_rerank_sigv4_target
```

### Bedrock model names are stripped before base-model resolution

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

### Azure fine-tuning responses are normalized to OpenAI shape

Azure fine-tuning jobs do not return the exact OpenAI shape. LiteLLM rewrites
`organization_id: null` to `""`, `result_files: null` to `[]`, and maps Azure
status values like `pending`, `notRunning`, and `canceling` into the closest
OpenAI statuses before constructing the public fine-tuning job object.

Design rule:

```text
Fine-tuning jobs need provider-specific status and field normalization.
```

Track:

```text
azure_finetuning_status_map
azure_finetuning_org_id_default
azure_finetuning_result_files_default
```

## 29. Twenty-first audit additions

### A2A agent cards are capability-filtered proxy surfaces

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

## 30. Twenty-second audit additions

### Router fallback paths are stripped and revalidated as security-sensitive inputs

The router removes internal `mock_testing_*` flags before dispatching the
request. Those flags exist only for router test paths and are explicitly
kept out of normal request handling. Separately, fallback model names are
revalidated against the API key allowlist, including names nested under
`router_settings_override`, because a caller could otherwise smuggle a
restricted model through a fallback path.

Design rule:

```text
Fallback and test-only router inputs are not benign metadata; they are security-sensitive routing state.
```

Track:

```text
mock_testing_flag_strip
fallback_model_allowlist_check
router_settings_override_validation
restricted_model_smuggle_prevention
```

## 31. Twenty-third audit additions

### OpenAI vector-store metadata is schema-filtered, not pass-through

The OpenAI vector-store helpers run `metadata` and file `attributes` through
`add_openai_metadata()` before dispatch. That helper strips `hidden_params`,
keeps only string-valued keys, and truncates the visible metadata down to 16
keys. So these requests are not a generic JSON passthrough; they are a
bounded metadata surface with a hidden/internal split.

Design rule:

```text
OpenAI metadata surfaces must be filtered for visibility and size before they hit the provider boundary.
```

Track:

```text
openai_vector_store_metadata_filter
openai_vector_store_attributes_filter
openai_metadata_16_key_cap
openai_metadata_string_only
```

### NVIDIA Riva transcription is a gRPC bridge with its own endpointing model

The Riva transcription adapter does not send OpenAI audio requests over HTTP.
It builds a structured gRPC payload instead, translating `language` into
`language_code`, turning `timestamp_granularities=["word"]` into
`enable_word_time_offsets`, and mapping OpenAI-style `chunking_strategy` into
Riva `endpointing_config`. It also leaves `model` empty by default so Riva can
auto-select a deployment from the language and sample rate.

The response side is equally special: the handler reassembles only final gRPC
results, optionally reconstructs word timestamps, and computes duration from
the stream for verbose JSON output.

Design rule:

```text
gRPC audio adapters need explicit request translation and stream reassembly, not a fake HTTP shim.
```

Track:

```text
nvidia_riva_language_code_mapping
nvidia_riva_word_time_offsets
nvidia_riva_endpointing_config_bridge
nvidia_riva_final_result_reassembly
```

### OpenAI video IDs and query variants are rewritten defensively

The OpenAI video adapter decodes LiteLLM-managed encoded character IDs back to
the original upstream IDs before dispatch, then re-encodes returned video IDs
with provider/model affinity on the way out. Content fetches also treat the
`variant` query as user-controlled input and quote it before appending to the
URL so it cannot smuggle extra query parameters.

Design rule:

```text
Video asset identifiers and download variants are transport data, not plain strings.
```

Track:

```text
openai_video_character_id_decode
openai_video_id_reencode
openai_video_variant_query_quote
openai_video_affinity_encoding
```

## 32. Twenty-fourth audit additions

### Databricks chat rewrites request shape before it ever reaches the model

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

### Cohere v2 forces single-step mode when tool results sit behind user history

The Cohere v2 chat adapter rewrites `tool_results` into the current request,
but if the last entry in `chat_history` is a `USER` message it also injects
`force_single_step=True` because the upstream API fails otherwise. That means
the request shape depends on the history tail, not just the visible current
turn.

Design rule:

```text
History tail can change the provider execution mode.
```

Track:

```text
cohere_force_single_step_on_user_tail
cohere_tool_results_history_dependency
```

### Gemini countTokens strips unsupported functionResponse IDs

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

### Gemini realtime drops unknown event types instead of forwarding them

The Gemini realtime bridge does not preserve every OpenAI event verbatim. If
the incoming message is not `session.update`, `response.create`,
`conversation.item.create`, or `input_audio_buffer.append`, the adapter
returns an empty list and intentionally drops the event rather than passing
raw JSON through to the backend.

Design rule:

```text
Realtime bridges need an explicit unknown-event policy.
```

Track:

```text
gemini_realtime_unknown_event_drop
gemini_realtime_event_whitelist
```

### OpenAI-like chat rewrites `max_completion_tokens` for generic compatibility

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

### Predibase chat rewrites sampling knobs into Hugging Face inference fields

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

### Black Forest Labs image generation maps OpenAI knobs into model-specific controls

Black Forest Labs does not treat OpenAI image parameters as universal. For
Ultra models, `n` is rewritten to `num_images`, `quality=hd` becomes `raw=True`,
and `size` is converted into explicit width/height pairs. For non-Ultra
models, `n` is silently ignored because the provider already defaults to a
single image.

The adapter also strips the provider prefix from the model name before looking
up the actual BFL endpoint, so routing depends on a normalized internal model
name rather than the public OpenAI-style one.

Design rule:

```text
Image adapters need per-model parameter gates, not one shared OpenAI schema.
```

Track:

```text
bfl_ultra_num_images_mapping
bfl_quality_hd_to_raw
bfl_size_to_width_height
bfl_non_ultra_n_skip
bfl_model_prefix_strip
```

### Sagemaker completion turns OpenAI prompts into Hugging Face inference payloads

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

### Azure realtime uses a two-step handshake instead of one live endpoint

Azure realtime does not expose a single websocket URL. The adapter splits the
flow into a `client_secrets` bootstrap call and a separate `calls` URL, both
with an `api-version` query string. The first step uses the configured Azure
API key, while the live call path switches to an ephemeral `api-key` header.

That means realtime auth is staged: the proxy has to obtain a client secret
before it can talk to the live session endpoint.

Design rule:

```text
Realtime endpoints may need a bootstrap call and a separate live channel.
```

Track:

```text
azure_realtime_client_secrets_url
azure_realtime_calls_url
azure_realtime_ephemeral_api_key
azure_realtime_two_step_handshake
```

### Gemini realtime buffers standalone usage metadata for the next response

Gemini Live can emit `usageMetadata` in its own frame, separate from the
content delta or tool-call frame. The realtime adapter treats that frame as a
benign no-op for output, but it buffers the usage payload so the next
`response.done` can consume it and keep spend accounting accurate. Without
that buffer, the same turn could look like zero spend and bypass budget
enforcement.

Design rule:

```text
Usage metadata may arrive out of band and still needs to be attributed exactly once.
```

Track:

```text
gemini_realtime_usage_metadata_buffer
gemini_realtime_standalone_usage_frame
gemini_realtime_usage_attribution
```

### Databricks Responses strips provider prefixes and stays HTTP-only

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

### Cloudflare AI Run returns a provider-native result payload, not an OpenAI envelope

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

### Azure image edit resolves auth with Azure-style header precedence

The Azure image-edit adapter no longer treats `Authorization: Bearer <api_key>`
as the default. It delegates to the shared Azure auth helper so the request
uses the Azure-style `api-key` header when possible and falls back to AAD
bearer auth only when that is the configured path. It also treats
`litellm_params["api_key"]` as the source of truth and only copies the
positional `api_key` argument when the params object is empty.

Design rule:

```text
Azure adapters should resolve auth the same way as the rest of the Azure family, not as direct OpenAI calls.
```

Track:

```text
azure_image_edit_api_key_precedence
azure_image_edit_api_key_header
azure_image_edit_aad_fallback
azure_image_edit_shared_azure_auth_helper
```

### OpenAI Responses token counting is its own schema bridge

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

### OpenAI text completions retain the raw upstream response in hidden params

The OpenAI text-completion wrapper does not just convert `choices[].text` into
chat-style messages. On the async path it also stashes the exact raw response
JSON into `_hidden_params.original_response`, so downstream code can inspect
the original upstream payload after the normalized response has been built.

Design rule:

```text
If the proxy normalizes a legacy completion format, keep the raw payload available for debugging and replay.
```

Track:

```text
openai_text_completion_original_response_hidden
openai_text_completion_async_payload_retention
```

### Gemini video rewrites one OpenAI size knob into both aspect ratio and resolution

Gemini Veo does not treat `size` as a single passthrough field. For known
OpenAI sizes, the adapter maps it to Gemini `aspectRatio` and, when the edge
size matches a supported preset, also infers a concrete `resolution` such as
`720p` or `1080p`. The same request also normalizes `seconds` into
`durationSeconds`, with a default of 4 seconds when the caller omits it.

Design rule:

```text
Video adapters may derive multiple downstream fields from a single upstream size hint.
```

Track:

```text
gemini_video_size_to_aspect_ratio
gemini_video_size_to_resolution
gemini_video_duration_seconds_default
```

### OpenAI container creation is billed as a code interpreter session

The OpenAI container create response is not just an object store record. After
parsing the returned container, the adapter injects a hidden response cost
derived from one code-interpreter session into
`_hidden_params["additional_headers"]["llm_provider-x-litellm-response-cost"]`.
That means container creation participates in cost accounting even though the
API surface itself looks like a plain resource create call.

Design rule:

```text
Resource creation endpoints can still be billable tool setup.
```

Track:

```text
openai_container_create_billed_session
openai_container_code_interpreter_cost
openai_container_cost_hidden_header
```

### OpenAI realtime HTTP uses a bootstrap URL and a separate live-call URL

OpenAI realtime does not ride on one endpoint. The HTTP helper builds a
`/v1/realtime/client_secrets` URL for bootstrap and a separate
`/v1/realtime/calls` URL for the live channel, while preserving the base path
trim logic for `/v1` roots. That split matters because session setup and live
traffic are different protocol phases, not one generic request.

Design rule:

```text
Realtime transport can be staged across multiple HTTP URLs, not just one websocket target.
```

Track:

```text
openai_realtime_client_secrets_url
openai_realtime_calls_url
openai_realtime_bootstrap_split
```

### Fireworks image input gets a `#transform=inline` URL rewrite for non-vision models

The Fireworks chat adapter mutates image URLs by appending
`#transform=inline` when the model is not a vision model and the feature is
not disabled. It deliberately skips `data:` URLs, because adding the fragment
to base64 payloads corrupts the inline image and breaks decoding on the
provider side.

Design rule:

```text
Image transport rewrites must respect whether the payload is already inline.
```

Track:

```text
fireworks_transform_inline_image_url
fireworks_skip_data_url_fragment
fireworks_non_vision_inline_rewrite
```

### Volcengine Responses repairs missing `response.output` before validation

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

### Manus Responses is agent-mode by default and fakes stream for async work

Manus does not behave like a generic OpenAI Responses backend. The adapter
forces `task_mode: "agent"` into the request body, extracts an `agent_profile`
from the model name, and marks the route as streaming even though the provider
does not support true realtime streaming. When the response comes back, it
also normalizes Manus-specific casing and fills in missing `reasoning`, `text`,
`output`, `usage`, and `id` fields so the OpenAI response model can be built
reliably.

Design rule:

```text
Agent-style backends may need both request injection and response repair.
```

Track:

```text
manus_task_mode_agent
manus_agent_profile_from_model
manus_fake_streaming
manus_response_field_backfill
manus_created_at_camel_to_snake
```

### xAI realtime speaks the OpenAI websocket shape without the beta header

xAI's Grok Voice Agent API reuses the OpenAI realtime websocket protocol, but
its handler deliberately sends only the `Authorization` header and skips the
`OpenAI-Beta: realtime=v1` header entirely. In other words, the wire shape is
OpenAI-like, but the protocol version negotiation is not the same as OpenAI's
beta path.

Design rule:

```text
Realtime compatibility does not imply the same version header contract.
```

Track:

```text
xai_realtime_no_beta_header
xai_realtime_openai_shape
```

## 17. One-line summary

The clean architecture is:

```text
Protocol -> CoreChat -> Provider
```

But the reliable implementation is:

```text
Protocol -> CoreChat state machine -> Provider quirks at the edge
```

## 18. Thirteenth audit additions

### ChatGPT subscription state is synthesized from auth and call metadata

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

## 19. Fourteenth audit additions

### Vertex partner-model token counting strips version suffixes

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

## 20. Fifteenth audit additions

### Vertex Anthropic forces a tool-based structured-output path by lying about the model

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

## 21. Sixteenth audit additions

### Vertex Anthropic strips malformed or unsupported `output_config` payloads

The Vertex AI Claude helper sanitizes `output_config` in place before the
request is sent upstream. Non-dict values are dropped outright, unsupported
keys are removed, and an `output_config` that becomes empty after filtering is
deleted instead of being sent as `{}`. The companion `output_format` field is
left alone.

Design rule:

```text
Provider-specific structured-output knobs may need a preflight sanitizer that
deletes invalid containers instead of forwarding empty shells.
```

Track:

```text
vertex_anthropic_output_config_sanitize
vertex_anthropic_drop_malformed_output_config
vertex_anthropic_remove_empty_output_config
```

## 22. Seventeenth audit additions

### Vertex Agent Engine always routes through `:streamQuery` and fabricates a user ID

Vertex AI Agent Engine does not expose a separate “chat completion” style
route in the adapter. The transformer always targets the `:streamQuery`
endpoint, even for non-streaming calls, and treats `stream_query` as the
payload method in both cases. It also maps OpenAI `user` into `user_id` and,
when neither is supplied, synthesizes a stable fallback like
`litellm-user-<random>` so the session-management API still gets an identity.

Design rule:

```text
Some agent runtimes want a single query route plus an always-present user
identity, even when the caller did not provide one.
```

Track:

```text
vertex_agent_engine_stream_query_only
vertex_agent_engine_user_to_user_id
vertex_agent_engine_synth_user_id
```

## 23. Eighteenth audit additions

### Anthropic legacy thinking gets rewritten into adaptive thinking

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

## 24. Nineteenth audit additions

### Gemini agents refuse custom `api_base` unless the caller supplies an explicit key

The Gemini Agents adapter treats `api_base` override as a security boundary.
If the caller points the request at a custom host without also providing an
explicit `api_key`, the adapter refuses to fall back to process-wide Google
keys. That prevents the proxy from shipping a shared `x-goog-api-key` header
to an attacker-controlled endpoint.

Design rule:

```text
When the destination host changes, inherited provider credentials should not
silently follow.
```

Track:

```text
gemini_agents_custom_api_base_key_required
gemini_agents_no_env_key_fallback_on_custom_base
gemini_agents_shared_key_leak_guard
```

## 25. Twentieth audit additions

### Gemini rewrites `citationSources` into `citations` on the response path

The Gemini generate-content response adapter mutates `citationMetadata` in
place. If the provider returns `citationSources`, LiteLLM renames that field to
`citations` so the downstream schema matches the expected response shape.

Design rule:

```text
Provider-native field names may need to be renamed on the way back so the
shared response model sees the schema it expects.
```

Track:

```text
gemini_citation_sources_to_citations
gemini_citation_metadata_response_rewrite
```

## 26. Twenty-first audit additions

### Azure base URLs can override `api-version` and force `/openai/v1`

Azure's common URL builder treats an `api-version` already embedded in
`api_base` as authoritative. If the base URL already has `api-version`, the
adapter leaves it alone instead of overwriting it from `litellm_params`. It
also rewrites `/openai` to `/openai/v1` when the selected API version is a
v1-style route such as `latest`, `preview`, or `v1`.

Design rule:

```text
Request-level version fields should not silently override a version baked into
the base URL, and v1-style Azure routes may need path normalization too.
```

Track:

```text
azure_api_version_from_base_url
azure_openai_v1_path_normalization
azure_base_url_api_version_precedence
```

## 27. Twenty-second audit additions

### xAI chat repairs tool-call finish reasons and folds usage back into OpenAI shape

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

## 28. Twenty-third audit additions

### Mistral response content lists are collapsed into text and reasoning content

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

## 29. Twenty-fourth audit additions

### Vercel AI Gateway nests provider-specific options under `extra_body`

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

## 30. Twenty-fifth audit additions

### xAI Responses strips unsupported top-level fields and rewrites tool definitions

The xAI Responses adapter is not a blind OpenAI passthrough. It drops
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

## 31. Twenty-sixth audit additions

### Baseten switches base URLs when the model name looks like a dedicated deployment

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

## 32. Twenty-seventh audit additions

### Recraft image edit collapses multi-image input to a single file

The Recraft image-edit adapter does not preserve OpenAI-style multi-image
input. If the caller passes a list of images, the adapter takes only the first
one because Recraft expects a single `image` file part. It also injects a
default `strength` of `0.2` when the caller omits one, so request shape is
normalized before multipart assembly.

Design rule:

```text
When a provider only accepts one source image, the bridge has to choose and
document which image survives if the caller sends more than one.
```

Track:

```text
recraft_image_edit_single_image_only
recraft_image_edit_first_image_wins
recraft_image_edit_default_strength
```

## 33. Twenty-eighth audit additions

### Parallel AI search remaps unified query/filter fields into `objective` and `source_policy`

The Parallel AI search adapter does not forward the generic search request
shape unchanged. A list of query strings is collapsed into a single
`objective`, while the unified domain filters are rewritten into
`source_policy.allowed_domains` and `source_policy.disallowed_domains`.
Everything else is passed through only after that provider-specific mapping,
so the adapter is doing a real search-protocol translation rather than a thin
HTTP proxy.

Design rule:

```text
Search adapters often need to translate both query intent and source policy
before the upstream API can understand the request.
```

Track:

```text
parallel_ai_search_objective_from_query_list
parallel_ai_search_source_policy_domain_mapping
parallel_ai_search_query_pass_through_after_remap
```

## 34. Twenty-ninth audit additions

### Novita injects a source header into every request

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

### Parallel AI search requires a beta feature header

Parallel AI search is gated behind a specific beta header. The adapter always
adds `parallel-beta: search-extract-2025-10-10` when building the request,
which means the feature contract is not just the endpoint path and body
shape; it also depends on a transport header.

Design rule:

```text
When an API feature is behind a beta header, that header belongs in the
normalized contract alongside the path and body mapping.
```

Track:

```text
parallel_ai_search_beta_header
parallel_ai_search_transport_gating
```

## 35. Thirtieth audit additions

### SambaNova embeddings require an explicit base URL and append `/embeddings`

The SambaNova embedding adapter does not synthesize its own base URL. It
requires `api_base` from the caller, then normalizes that value by stripping
trailing slashes and appending `/embeddings` if needed. That means the caller
has to supply the deployment location up front, and the adapter owns the final
endpoint shape.

Design rule:

```text
Some embedding backends treat the base URL as mandatory input rather than a
fall-backable default, and the adapter still has to normalize the final path.
```

Track:

```text
sambanova_embeddings_api_base_required
sambanova_embeddings_append_path
sambanova_embeddings_no_default_base
```
