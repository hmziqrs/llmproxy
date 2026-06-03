# Mini Quirks — Endpoints: Embeddings & Rerank

## Embedding dimensions are not portable

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

## Bedrock embeddings strip auth params and default the region

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

## Bedrock rerank rewrites the runtime host to agent-runtime

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

## SambaNova embeddings require an explicit base URL and append `/embeddings`

The SambaNova embedding adapter does not synthesize its own base URL. It
requires `api_base` from the caller, then normalizes that value by stripping
trailing slashes and appending `/embeddings` if needed.

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

## Hosted vLLM embeddings require a base URL, strip the provider prefix, and ignore fake auth

The Hosted vLLM embedding adapter follows the same local-runtime pattern as
the other hosted-vLLM surfaces. It requires `api_base`, appends `/embeddings`
if needed, strips the `hosted_vllm/` prefix from the model name before
sending the request, and uses `fake-api-key` only as a placeholder so the
OpenAI client path can proceed without a real credential. If a real API key is
present, it is forwarded; otherwise the Authorization header is omitted.

Design rule:

```text
Embedding adapters for local OpenAI-compatible runtimes may still need explicit
base URL handling, model-name normalization, and placeholder auth behavior.
```

Track:

```text
hosted_vllm_embedding_api_base_required
hosted_vllm_embedding_model_prefix_strip
hosted_vllm_embedding_fake_api_key
```

## DeepInfra rerank unwraps nested JSON error details before surfacing failures

The DeepInfra rerank adapter also changes the failure contract. If the upstream
error payload is JSON and contains `{"detail": {"error": "..."}}`, the
adapter unwraps that nested field and surfaces the inner error text instead of
the raw wrapper. It does the same for a string `detail` field.

Design rule:

```text
Failure paths are part of the adapter contract too, and nested provider error
envelopes may need to be collapsed before they reach the caller.
```

Track:

```text
deepinfra_rerank_error_detail_unwrap
deepinfra_rerank_error_message_normalization
```

## Nvidia NIM rerank rewrites the rerank contract into object-based query and passage fields

Nvidia NIM rerank does not accept the shared rerank shape directly. The
adapter wraps the query as `{"text": ...}`, converts each document into a
`passages[*].text` item, maps Cohere's `top_n` to Nvidia's `top_k`, and strips
the `nvidia_nim/` model prefix before building the URL. It also converts
underscores back to periods for the model field in the request body, so the
model identifier is normalized differently for routing and payload.

Design rule:

```text
Rerank backends often split logical query/document inputs into provider
objects, and the model name may need one normalization for the URL and another
for the JSON body.
```

Track:

```text
nvidia_nim_rerank_query_object
nvidia_nim_rerank_passages_text
nvidia_nim_rerank_top_n_to_top_k
nvidia_nim_rerank_model_prefix_and_underscore_normalization
```

## Nvidia NIM embeddings tuck provider-specific knobs into `extra_body`

Nvidia NIM embeddings keep the OpenAI embedding surface, but they route
provider-specific knobs through `extra_body` instead of top-level fields.
`input_type` and `truncate` are lifted into that escape hatch, and any extra
kwargs passed by the caller are merged there too. That preserves the OpenAI
shape while still allowing NIM-only parameters to survive dispatch.

Design rule:

```text
Embedding adapters often need a structured escape hatch for provider-specific
knobs that do not belong on the shared OpenAI surface.
```

Track:

```text
nvidia_nim_embedding_extra_body_knobs
nvidia_nim_embedding_input_type
nvidia_nim_embedding_truncate
```

## IBM WatsonX embeddings rewrite `input` to `inputs` and build deployment-specific routes

The WatsonX embedding adapter does not forward the OpenAI embedding body as-
is. It renames the request field to `inputs`, keeps the remaining options in
`parameters`, and builds either the shared embeddings endpoint or a
deployment-specific endpoint depending on the model prefix. On the way back, it
maps WatsonX `results[*].embedding` into OpenAI-style embedding objects and
turns `input_token_count` into the usage block.

Design rule:

```text
Embedding bridges often need a request-field rename plus a route selector that
changes when the model name refers to a deployment.
```

Track:

```text
watsonx_embedding_inputs_field
watsonx_embedding_deployment_endpoint
watsonx_embedding_response_object_rewrite
```

## GitHub Copilot Responses and embeddings synthesize headers and preserve encrypted reasoning state

The GitHub Copilot Responses
adapter authenticates through OAuth device flow, injects a fixed Copilot header
set, derives `X-Initiator` from the request content, and adds a vision header
when the input contains images. On the response path, reasoning items keep
`encrypted_content` intact while dropping `status=None`, because the encrypted
blob is required for later turns. The embeddings adapter also strips the
`github_copilot/` prefix from model IDs before sending the request.

Design rule:

```text
Header synthesis and encrypted reasoning state are part of the adapter
contract, not incidental auth plumbing.
```

Track:

```text
github_copilot_oauth_header_synthesis
github_copilot_x_initiator_from_input
github_copilot_vision_header_detection
github_copilot_reasoning_encrypted_content_preserved
github_copilot_embedding_model_prefix_strip
```

## SageMaker embeddings switch payload shape by model family and accept multiple response envelopes

SageMaker embeddings are not one fixed wire format. The generic Hugging Face
path rewrites OpenAI `input` into `inputs`, then accepts either a raw list of
embedding vectors or a dict with an `embedding` field on the response side.
The Cohere path is different again: it converts the request into `texts`
and `input_type`, preserves `input_type` when provided, and reuses Cohere’s
response population logic instead of the HF shape.

Design rule:

```text
Embedding adapters may need a model-family switchboard because different
containers on the same host still speak different payload dialects.
```

Track:

```text
sagemaker_embedding_inputs_plural
sagemaker_embedding_raw_array_or_embedding_dict
sagemaker_embedding_model_family_factory
sagemaker_cohere_texts_and_input_type
sagemaker_cohere_response_population
```

## Voyage AI uses different request shapes for embeddings and rerank, with token-based usage accounting

Voyage is not a single generic vector API. The embedding adapter sends the
OpenAI `input` field plus `model`, remaps `dimensions` to `output_dimension`,
and accepts multiple API-key environment names (`VOYAGE_API_KEY`,
`VOYAGE_AI_API_KEY`, `VOYAGE_AI_TOKEN`). The rerank adapter similarly rewrites
`top_n` to `top_k`, returns `results` from the provider’s `data` field, and
normalizes string-shaped `document` echoes into `{"text": ...}`. Both paths
derive usage from the provider’s `total_tokens` counter.

Design rule:

```text
Vector and rerank providers often need their own response-shape translation
and token accounting even when their top-level surface feels similar.
```

Track:

```text
voyage_embedding_env_aliases
voyage_embedding_dimensions_to_output_dimension
voyage_rerank_top_n_to_top_k
voyage_rerank_string_document_normalization
voyage_usage_total_tokens_accounting
```

## Triton embeddings flatten a tensor response into OpenAI-style vectors

Triton’s embedding adapter is a tensor bridge rather than a JSON schema shim.
The request payload is a single `inputs` tensor named `input_text` with
`shape=[len(input)]`, `datatype="BYTES"`, and the raw input list as `data`.
The response path then reads `outputs`, takes each tensor’s `shape` and flat
`data` array, splits it back into individual embeddings, and renumbers the
results from zero. It also rewrites the reported model name from
`model_name` and computes usage locally with `token_counter`, falling back to a
whitespace estimate when tokenization fails.

Design rule:

```text
Tensor backends need explicit flatten/unflatten logic and local usage
estimation because the provider response does not speak OpenAI natively.
```

Track:

```text
triton_embedding_tensor_request
triton_embedding_output_split_by_shape
triton_embedding_model_name_remap
triton_embedding_local_usage_estimate
```

## Hosted VLLM rerank normalizes the base URL, injects fake auth, and rebuilds rerank meta from total tokens

Hosted VLLM rerank is its own protocol surface, not just a Cohere clone. The
adapter normalizes the base URL to `/rerank`, preserving backward compatibility
with callers that still point at `/v1/rerank`. It also injects a fake bearer
token when no API key is present, so the OpenAI-style client stack can run
against a local runtime without real auth. On the request side it accepts the
shared `query` / `documents` / `top_n` / `rank_fields` / `return_documents`
shape, but explicitly rejects `max_chunks_per_doc`. On the response side it
rebuilds rerank meta from the provider’s `usage.total_tokens`, requires
`results[*].index` and `results[*].relevance_score`, and preserves any echoed
document text when present while fabricating an ID if the provider omits one.

Design rule:

```text
Local rerank runtimes still need explicit route normalization and fake auth to stay OpenAI-compatible.
```

Track:

```text
hosted_vllm_rerank_route_normalization
hosted_vllm_rerank_fake_api_key
hosted_vllm_rerank_max_chunks_rejected
hosted_vllm_rerank_usage_from_total_tokens
hosted_vllm_rerank_fabricated_id
```
