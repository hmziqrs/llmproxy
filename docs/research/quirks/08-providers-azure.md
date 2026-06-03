# Mini Quirks — Provider Quirks: Azure

## Azure Assistants backfills missing message status

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

## Azure Responses O-series drops unsupported temperature

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

## Azure Responses strips reasoning status fields

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

## Azure base URLs can override `api-version` and force `/openai/v1`

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

## Azure AI Agents bridges a thread/run workflow into OpenAI chat completions and rewrites citations on the way back

Azure AI Agents is a multi-step assistant runtime, not a direct chat
completion. The adapter extracts the agent ID from `azure_ai/agents/<id>` or
`agent_id`/`assistant_id`, then drives the API as a thread/run workflow:
create thread, add user/system messages, create a run, poll run status, and
finally list thread messages. The request path also carries an `api_version`
field and uses Azure AD bearer auth rather than an API key. On the response
side it rebuilds an OpenAI-style assistant message from the thread’s assistant
text, converts Azure `url_citation` annotations into OpenAI-compatible
annotations by moving `start_index` and `end_index` into `url_citation`, and
stores the `thread_id` in hidden params for conversation continuity. The
streaming path translates native SSE events into OpenAI chat chunks, collects
citations from `thread.message.completed`, and emits a final `[DONE]` chunk with
the hidden `thread_id`.

Design rule:

```text
Agent runtimes need their own orchestration layer and a response bridge that reconstructs chat-style output from thread state.
```

Track:

```text
azure_ai_agents_thread_run_orchestration
azure_ai_agents_bearer_auth
azure_ai_agents_url_citation_rewrite
azure_ai_agents_thread_id_hidden_state
azure_ai_agents_sse_event_to_chat_chunk_bridge
```
