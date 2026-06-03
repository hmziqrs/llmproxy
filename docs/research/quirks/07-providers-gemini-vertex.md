# Mini Quirks — Provider Quirks: Gemini / Vertex

## Gemini response schema keys are capability-swapped

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

## Google GenAI request bodies rename `generationConfig` to `config`

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

## Gemini system instructions are represented as synthetic turns

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

## Gemini agents refuse custom `api_base` unless the caller supplies an explicit key

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

## Gemini rewrites `citationSources` into `citations` on the response path

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
