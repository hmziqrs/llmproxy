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

### Routing affinity is real

LiteLLM is the only reference with serious `previous_response_id`,
container, encrypted-content, and deployment affinity handling.

Design rule:

```text
Treat response IDs and container IDs as routing metadata, not just strings.
```

For v1 this can be deferred. For Responses API, it cannot.

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

### Image generation has model-dependent endpoints

Gemini image generation uses `:generateContent` for Gemini Flash image
preview models but `:predict` for Imagen models. OpenAI `size` maps to
provider aspect ratios, and usage can contain modality-specific token
details.

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

## 15. One-line summary

The clean architecture is:

```text
Protocol -> CoreChat -> Provider
```

But the reliable implementation is:

```text
Protocol -> CoreChat state machine -> Provider quirks at the edge
```
