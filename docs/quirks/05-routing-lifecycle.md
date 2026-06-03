# Mini Quirks — Routing, Fallback & Lifecycle

## Routing affinity is real

LiteLLM is the only reference with serious `previous_response_id`,
container, encrypted-content, and deployment affinity handling.

Design rule:

```text
Treat response IDs and container IDs as routing metadata, not just strings.
```

For v1 this can be deferred. For Responses API, it cannot.

## Cancellation and cleanup

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

## Router and fallback must share one chain

One reference has router fallback logic and circuit-breaker fallback
logic that can diverge.

Design rule:

```text
The router produces one ordered execution plan.
The executor consumes that exact plan.
```

Do not let the executor recompute fallback order.

## Duplicate request suppression

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

## OpenRouter packs route knobs into `extra_body`

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

## Scenario routing is heuristic and request-shape dependent

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

## Retry-after parsing is provider archaeology

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

## Provider routes are canonical identity

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

## Pass-through logging is route-specific

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

## Multimodal blocks need content-type routing

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

## Gemini MIME types are normalized before file routing

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

## Claude Platform on AWS is a routed Bedrock variant

The Claude Platform path strips the
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

## Bedrock Invoke Agent is its own route and trace protocol

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

## Background side effects need durability classes

The audited code splits side effects into different durability tiers:

- logging work is best-effort, bounded, and allowed to drop or retry later
- buffered writes are retryable and intended to survive process exit
- stateful writes keep an in-memory truth even when disk is unhealthy
- stream cleanup is attempted in `finally`, but client disconnect or shutdown can still cut it short

Design rule:

```text
Every side effect must declare its durability class.
```

Track:

```text
durability = drop | retry | flush_on_exit | persist_until_success
side_effect_kind = log | usage_state | credential_state | cache_write | stream_cleanup
```

## Shutdown is a first-class state

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

## Disk health and memory truth can diverge

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

## Anthropic experimental pass-through can reroute thinking to Responses

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

## Streaming output items are re-identified for follow-up routing

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

## Router fallback paths are stripped and revalidated as security-sensitive inputs

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

## Vertex Agent Engine always routes through `:streamQuery` and fabricates a user ID

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

## DataRobot rewrites the API base into LLMGW unless the path is already a deployment route

The DataRobot adapter does not leave the incoming base URL alone. If the path
is empty, it appends `/api/v2/genai/llmgw/chat/completions/`. If the path is a
standard `api/v2` endpoint, it appends the same LLMGW suffix. But if the path
already points at `api/v2/deployments`, it preserves that deployment route
instead of forcing the shared gateway path. The adapter also falls back to a
fake API key when no token is available, so both route and auth resolution are
adapter-owned.

Design rule:

```text
Route normalization can be conditional on whether the caller is targeting a
shared gateway or a dedicated deployment host.
```

Track:

```text
datarobot_llmgw_path_injection
datarobot_deployment_path_passthrough
datarobot_fake_api_key_fallback
```

## Hosted vLLM Responses routes to `/v1/responses` and uses a fake key when auth is absent

The Hosted vLLM Responses adapter normalizes the base URL into the
`/v1/responses` endpoint, adding `/v1` when the caller did not already provide
it. It also falls back to `fake-api-key` if neither the caller nor the
environment supplies one, because the local OpenAI client wrapper expects an
Authorization header even when the backend itself does not require auth.
Native WebSocket transport is also disabled for this adapter.

Design rule:

```text
Responses bridges still need explicit route normalization and a local-runtime
auth shim when the backend is OpenAI-compatible but not auth-requiring.
```

Track:

```text
hosted_vllm_responses_route_append
hosted_vllm_responses_fake_api_key
hosted_vllm_responses_http_only
```

## Gemini rewrites media payloads, injects fallback text, and mutates cached-content requests

Gemini does a lot more than parse OpenAI messages. It rewrites media URLs
into `file_data` / `inline_data`, resolves `gs://` metadata when MIME type is
missing, injects a blank user part or even a default user message when the
request would otherwise be textless or system-only, promotes the highest
image detail into global `generationConfig.mediaResolution` on Gemini 2.x,
drops `system_instruction` / `tools` / `toolConfig` when `cachedContent` is
present unless mutation is explicitly allowed, and rewrites
`service_tier=default` to `serviceTier=standard`.

Design rule:

```text
Gemini request normalization spans media parsing, fallback message synthesis,
and cached-content compatibility guards.
```

Track:

```text
gemini_media_url_to_file_or_inline
gemini_gcs_mime_resolution_lookup
gemini_empty_text_fallback_message
gemini_system_only_default_user
gemini_generation_config_media_resolution
gemini_cached_content_request_sanitization
gemini_service_tier_standard_rewrite
```

## Azure AI Studio chat normalizes the route, promotes extra-body fields, and rewrites the response model namespace

Azure AI Studio chat is not a one-route passthrough. The adapter chooses
`/models/chat/completions` when the base URL is already under
`services.ai.azure.com`, otherwise it falls back to `/chat/completions`.
Before dispatch it promotes Anthropic-style or provider-specific fields out of
`extra_body`, drops `max_retries`, and then delegates to the parent OpenAI
request mapper. On the response side it rewrites the returned model into
`azure_ai/{model}` so downstream cost and display logic can distinguish Azure AI
Studio from plain Azure OpenAI. It also switches the auth mode between `api-key`
and bearer token based on the host and whether the model is an Azure OpenAI
model.

Design rule:

```text
Hybrid Azure chat surfaces need route normalization, request cleanup, and a response model namespace rewrite.
```

Track:

```text
azure_ai_studio_models_chat_completions_route
azure_ai_studio_chat_extra_body_promotion
azure_ai_studio_chat_max_retries_strip
azure_ai_studio_response_model_namespace
azure_ai_studio_auth_mode_split
```

## Anthropic advisor orchestration is a synthetic tool loop with its own sub-call routing and streaming wrapper

The advisor tool is not executed natively for non-Anthropic providers. The
interceptor detects `advisor_20260301`, replaces it with a synthetic regular
function tool for the executor model, and then runs a hidden loop: the
executor call is always non-streaming, any `tool_use` named `advisor` triggers
a second advisor-model call, and the advisor result is injected back into the
message history before the executor is called again. The advisor sub-call can
carry its own `api_key` and `api_base`, and the handler stamps `parent_request_id`
plus `advisor_sub_call` metadata onto both legs. If the caller requested
streaming, the final executor response is wrapped in a fake Anthropic stream
iterator instead of returning the raw non-streaming payload.

Design rule:

```text
Agent-style helper tools are their own nested request graph, not just another tool choice.
```

Track:

```text
anthropic_advisor_synthetic_tool_rewrite
anthropic_advisor_subcall_routing_override
anthropic_advisor_parent_request_id_propagation
anthropic_advisor_nonstreaming_executor_loop
anthropic_advisor_fake_stream_wrapper
```
