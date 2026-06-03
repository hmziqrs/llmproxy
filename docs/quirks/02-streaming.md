# Mini Quirks — Streaming & Stream Events

## Stream errors after partial output

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

## Cached stream replay can double count

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

## Empty chunk filtering is protocol-specific

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

## Responses-style streams have event semantics, not chunk semantics

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

## Streaming tool calls are accumulated by index before they are emitted

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

## Google stream endpoints suppress OpenAI's `[DONE]` terminator

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

## Some handlers preserve original request context for downstream hooks

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

## ChatGPT backend tool-call streams need index repair and duplicate suppression

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

## Polling mode is a Redis snapshot of the stream, not a copied final response

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

## MCP streaming emits synthetic discovery and tool-execution events

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

## Failed or incomplete Response streams still materialize a completed response

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

## Responses stream iterators use a priority queue of synthetic events

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

## SSE recovery fabricates stable slots when the stream omits indices

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

## OpenAI text completions retain the raw upstream response in hidden params

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

## Manus Responses is agent-mode by default and fakes stream for async work

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

## Ollama streaming uses `<think>` boundaries to separate reasoning from visible text

The Ollama streaming iterator does not treat every chunk as visible assistant
output. When a streamed chunk includes `<think>`, the adapter marks the
reasoning section as started and routes subsequent text into
`reasoning_content` until it sees `</think>`. After that boundary closes, the
remaining text is emitted as normal assistant content. This is a stream-level
state machine, not a simple pass-through of chunks.

Design rule:

```text
Stream adapters sometimes need to track hidden reasoning boundaries across
multiple chunks before they can decide what is visible output.
```

Track:

```text
ollama_stream_think_boundary_reasoning
ollama_stream_think_boundary_visible_text
ollama_stream_reasoning_state_machine
```

## OVHCloud streaming normalizes the reasoning field during a migration window

OVHCloud’s streaming chat adapter is carrying a provider field migration in
flight. When streamed deltas contain `reasoning`, the adapter rewrites that
field back to `reasoning_content` so downstream consumers keep seeing the
older OpenAI-shaped key during the transition. The adapter also passes through
`extra_body` into the final request and uses a provider-specific error class,
but the field rewrite is the important protocol quirk here.

Design rule:

```text
When a provider renames a streamed field mid-migration, the bridge needs to
normalize the old and new names into one stable downstream contract.
```

Track:

```text
ovhcloud_stream_reasoning_content_migration
ovhcloud_stream_reasoning_field_alias
```

## SageMaker Nova removes `model` from the body and requires `stream` in the request payload

SageMaker Nova is not identical to the generic SageMaker chat bridge. The
adapter marks `stream` as a real request-body field, exposes Nova-specific
parameters such as `top_k`, `reasoning_effort`, `allowed_token_ids`, and
`truncate_prompt_tokens`, and then explicitly strips `model` from the payload
before dispatch. That means the model name is used for routing, not for the
upstream JSON body.

Design rule:

```text
Route identity and request-body identity are separate concerns on some
SageMaker deployments, and streaming can be a body-level flag.
```

Track:

```text
sagemaker_nova_stream_body_flag
sagemaker_nova_model_body_strip
sagemaker_nova_extra_param_allowlist
```

## CometAPI normalizes the base URL and aliases streamed reasoning back to `reasoning_content`

CometAPI is OpenAI-compatible, but it still has adapter logic that matters.
The chat path forces `/v1/chat/completions` onto the base URL unless it is
already present, tolerating several caller-provided base shapes. The streaming
iterator also rewrites `delta.reasoning` into `delta.reasoning_content` so
downstream consumers keep seeing the OpenAI-shaped field during streaming.
The adapter keeps a generic `extra_body` merge path as well, but the route and
stream field normalization are the real protocol quirks.

Design rule:

```text
Even “OpenAI-compatible” providers can need route normalization and streamed
field aliasing to preserve a stable downstream contract.
```

Track:

```text
cometapi_chat_route_normalization
cometapi_stream_reasoning_content_alias
cometapi_extra_body_merge
```

## HuggingFace completions infer task type, rebuild streamed output, and fan out best-of choices

HuggingFace’s completion adapter is not one fixed OpenAI clone. It first
infers a task from the model name or HuggingFace model metadata, choosing
between `conversational`, `text-generation-inference`, and a generic
text-generation path. That task choice changes the request envelope: the
conversational path builds `inputs.text` plus `past_user_inputs` and
`generated_responses`, while the TGI path sends a single prompt string with a
`parameters` object and a boolean `stream` flag. The adapter also maps
OpenAI-style `echo` into `decoder_input_details=True`, rewrites `max_tokens`
and `max_completion_tokens` into `max_new_tokens`, and coerces zero temperature
to `0.01` so the upstream HF runtime does not reject it. On the response side
it collapses SSE chunks into one synthetic `generated_text`, parses HF
`details.tokens` for logprob and finish-reason metadata, and expands
`details.best_of_sequences` into extra OpenAI choices when `best_of > 1`.

Design rule:

```text
HuggingFace adapters need task inference plus both stream collapse and choice fan-out,
because the provider exposes several incompatible completion shapes under one surface.
```

Track:

```text
huggingface_task_inference_from_model
huggingface_conversational_input_envelope
huggingface_tgi_stream_flag_and_parameters
huggingface_echo_to_decoder_input_details
huggingface_temperature_zero_floor
huggingface_best_of_sequences_to_choices
huggingface_streamed_response_collapse
```
