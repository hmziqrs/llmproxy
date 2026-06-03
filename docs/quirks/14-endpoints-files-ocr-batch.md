# Mini Quirks — Endpoints: Files, OCR, Vector Stores & Batch

## Container handlers must preserve URL query strings

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

## Embedding batching changes response assembly

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

## File upload protocols can be multi-step

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

Azure Foundry FLUX image generation uses a provider path instead of a normal
deployment path. For FLUX-2 models the adapter ignores the OpenAI-style
deployment URL pattern and builds
`/providers/blackforestlabs/v1/flux-2-pro?api-version=preview` unless the
caller already supplied a provider path. The model name is normalized from
variants like `flux.2-pro` to `flux-2-pro` so the route matches Azure’s
provider contract.

Design rule:

```text
Some image models route through a provider namespace, so the adapter must normalize model names into provider-specific paths instead of deployment paths.
```

Track:

```text
azure_flux2_provider_path
azure_flux2_model_name_normalization
azure_flux2_preview_api_version_default
azure_flux2_provider_path_passthrough
```

## Gemini file search is a generateContent bridge

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

## Emulated file_search is a synthetic two-step response

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

## Managed batch and fine-tune IDs are rewritten with model affinity

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

## Bedrock batch polling resolves region from the ARN

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

## Vector store searches carry provider-specific query semantics

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

## Bedrock rerank wraps sources as inline text or JSON documents and normalizes Bedrock result scores

The Bedrock rerank adapter does more than swap the host. On the request side it
converts each document into a Bedrock `INLINE` source, using
`textDocument`/`type="TEXT"` for string documents and `jsonDocument`/`type="JSON"`
for structured documents. It also builds a Bedrock reranking configuration with
a single text query, the target `modelArn`, and `numberOfResults` derived from
`top_n` or the full document count when `top_n` is missing. On the response
side it maps Bedrock `results[*].relevanceScore` back into LiteLLM
`relevance_score`, carries `usage` into rerank meta, and fabricates an `id`
when the provider does not return one.

Design rule:

```text
Bedrock rerank has its own source wrapper and score shape; it is not just a host rewrite.
```

Track:

```text
bedrock_rerank_inline_text_and_json_sources
bedrock_rerank_number_of_results_from_top_n
bedrock_rerank_relevance_score_mapping
bedrock_rerank_usage_meta
bedrock_rerank_fabricated_id
```

## Azure fine-tuning responses are normalized to OpenAI shape

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

## OpenAI vector-store metadata is schema-filtered, not pass-through

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

## OpenAI container creation is billed as a code interpreter session

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

## DeepInfra rerank duplicates the query across documents and hides runtime metadata

The DeepInfra rerank adapter does not accept a single query in the way the
shared rerank interface does. It repeats the query so `queries` matches the
number of documents, because that is what the upstream API expects. On the way
back, it also preserves provider-specific runtime, status, cost, and token
fields in `_hidden_params` instead of flattening them into the normal response
shape.

Design rule:

```text
Rerank adapters may need to expand one logical query into a provider-specific
array shape and retain the provider's execution metadata separately.
```

Track:

```text
deepinfra_rerank_query_duplication
deepinfra_rerank_hidden_runtime_metadata
deepinfra_rerank_hidden_cost
```

## DashScope rerank uses its own rerank route and normalizes string-shaped document echoes

DashScope rerank is a separate protocol surface. The adapter targets
`/compatible-api/v1/reranks` instead of the chat/embed compatibility routes,
accepts `return_documents=true` for `qwen3-rerank`, and normalizes string
document echoes back into `{"text": ...}` so the LiteLLM rerank response
shape stays stable.

Design rule:

```text
Rerank adapters often have endpoint and echo-shape quirks that do not match
the main chat API family.
```

Track:

```text
dashscope_rerank_route
dashscope_rerank_return_documents
dashscope_rerank_string_document_normalization
```

## HuggingFace rerank renames documents to texts and reconstructs document echoes on the way back

HuggingFace rerank is not a direct Cohere-shaped pass-through. The request
side renames `documents` to `texts`, renames `return_documents` to
`return_text`, and injects `raw_scores=False`, `truncate=False`, and
`truncation_direction="Right"` so the upstream rerank endpoint gets the shape
it expects. On the response side it takes provider items shaped like
`{index, score, text?}` and converts them into LiteLLM rerank results with
`relevance_score`, then rebuilds `document.text` either from the API’s echoed
text or from the original request document at the same index if HuggingFace
omitted it. The adapter also computes token usage locally from the query and
document text, with a fallback estimate if token counting fails.

Design rule:

```text
Rerank adapters often need separate request and response renaming, plus a
document-echo fallback when the provider only returns indices and scores.
```

Track:

```text
huggingface_rerank_documents_to_texts
huggingface_rerank_return_documents_to_return_text
huggingface_rerank_default_truncation_policy
huggingface_rerank_document_echo_fallback
huggingface_rerank_local_token_usage_estimate
```

## Reducto OCR resolves files into uploaded file IDs and switches request shape by API version

Reducto OCR is a file-ingestion bridge, not a direct document upload. The
adapter first extracts either a file ID or raw bytes from the input source; if
the input is not already an ID, it uploads the bytes to Reducto and uses the
returned ID in the request. The request shape then splits by API version:
`ParseV3` sends `{"input": <file_id>, ...}` with supported params such as
`formatting`, `retrieval`, and `settings`, while the legacy path sends
`{"document_url": <file_id>, "options": {"enhance": ...}}`. On the response
side it normalizes `result` into OCR pages, reads `usage.num_pages` and
`usage.credits`, and preserves the raw provider payload in hidden params.

Design rule:

```text
OCR adapters that front file processors need an upload/ID phase before they can build the final request body.
```

Track:

```text
reducto_ocr_extract_file_id_or_upload
reducto_ocr_v3_input_envelope
reducto_ocr_legacy_document_url_envelope
reducto_ocr_usage_num_pages_and_credits
reducto_ocr_hidden_raw_payload
```

## Vertex AI OCR rewrites remote document URLs into base64 data URIs before delegating to Mistral OCR

Vertex AI OCR is a request-shaping bridge over the Mistral OCR contract. It
requires a Vertex project and falls back to `us-central1` when no location is
provided, then builds the `:rawPredict` URL against the Mistral publisher
endpoint. On the payload side it inspects the incoming document object and, if
the document is a remote `document_url` or `image_url`, fetches that URL and
converts it into a base64 `data:` URI before handing the document off to the
Mistral OCR transformer. Both sync and async paths do the same conversion, so
the provider never needs to fetch the remote URL itself.

Design rule:

```text
Some OCR backends cannot dereference remote URLs, so the proxy must inline the bytes before the request leaves the process.
```

Track:

```text
vertex_ocr_project_required
vertex_ocr_default_location
vertex_ocr_rawpredict_url
vertex_ocr_document_url_to_data_uri
vertex_ocr_image_url_to_data_uri
vertex_ocr_sync_and_async_parity
```

## Azure AI Search vector store turns a query into an embedding-backed vector search and normalizes search hits

Azure AI Search vector-store search is a two-stage bridge. The adapter first
turns a list or string query into a single string, then generates an embedding
through `litellm.embedding` using a caller-supplied embedding model and
embedding config. That query vector is inserted into Azure’s
`vectorQueries` request body alongside `search="*"` and a `select` projection,
while the vector field name and `top_k` come from LiteLLM parameters. On the
response side it normalizes Azure AI Search hits from `value[]` into the shared
vector-store search schema, mapping `@search.score` to `score`, wrapping text in
`VectorStoreResultContent`, preserving extra fields as attributes, and copying
the document id into both `file_id` and `document_id`.

Design rule:

```text
Vector-store search adapters may need to embed the query first, then translate the provider's native hit schema back into a shared search result model.
```

Track:

```text
azure_ai_search_query_embedding_bridge
azure_ai_search_vector_queries_request
azure_ai_search_top_k_mapping
azure_ai_search_search_score_to_score
azure_ai_search_document_id_attributes
```

## Vertex AI DeepSeek OCR rewrites OCR requests into chat completions and reconstructs OCR pages from chat output

Vertex AI DeepSeek OCR is not a normal OCR endpoint. The adapter converts the
incoming OCR document into a chat-completions request, using a
`deepseek-ai/{model}` model name and wrapping the image or document URL inside a
single user message with `content` blocks. On the response side it reads the
chat completion output, tries to parse the assistant content as JSON, and falls
back to a single markdown page when the content is plain text. If the parsed
payload still does not contain pages, it wraps the content into a single page on
its own. The adapter also preserves usage info when present and carries through
document annotations from the parsed OCR payload.

Design rule:

```text
Some OCR backends are actually chat backends in disguise, so the proxy must synthesize and then unwrap chat completions around the OCR payload.
```

Track:

```text
vertex_deepseek_ocr_chat_request_bridge
vertex_deepseek_ocr_model_prefix_rewrite
vertex_deepseek_ocr_json_or_markdown_fallback
vertex_deepseek_ocr_single_page_wrap
vertex_deepseek_ocr_usage_info_passthrough
```

## Anthropic batch files stream JSONL results into OpenAI batch results and map Anthropic batch states

Anthropic batch file content is not a raw file download. The file-content
handler treats the batch ID as a file identifier, fetches
`/v1/messages/batches/{batch_id}/results`, and then rewrites Anthropic JSONL
records into OpenAI batch-result JSONL. Successful Anthropic message results
become OpenAI-style `response.body` payloads, errored results map Anthropic
error types onto HTTP status codes, and canceled or expired batches are emitted
as error-style batch outputs. The batch poller also maps Anthropic batch states
like `in_progress`, `canceling`, and `ended` back into OpenAI batch lifecycle
states with timestamps and request counts preserved.

Design rule:

```text
Batch-result bridges may need to translate both the per-record payload and the batch lifecycle state machine.
```

Track:

```text
anthropic_batch_results_jsonl_bridge
anthropic_batch_error_status_mapping
anthropic_batch_state_mapping
anthropic_batch_file_id_results_lookup
anthropic_batch_request_counts_rewrite
```

## Anthropic Files and Skills are separate beta surfaces with their own auth, URL, and object-shape bridges

Anthropic Files is not a pass-through file endpoint. The adapter injects
Anthropic-specific auth plus `anthropic-version` and
`anthropic-beta: files-api-2025-04-14`, uploads files as multipart form data,
defaults `purpose` to `messages`, and falls back to a timestamp-based filename
when the source payload has none. On the response side it converts Anthropic's
file object into OpenAI's file shape, including `size_bytes -> bytes`,
`created_at -> unix timestamp`, and a fixed `status="uploaded"`. List,
retrieve, delete, and content requests are all routed through `/v1/files/...`
with URL-encoded IDs, while content download stays binary.

Anthropic Skills is a separate surface with its own beta header. The adapter
requires Anthropic auth, sets `anthropic-version`, and merges
`anthropic-beta: skills-2025-10-02` into any existing beta header value without
losing caller-supplied flags. Skills requests are routed under `/v1/skills`,
create uses the request body directly, and list/get/delete are reshaped around
the skill ID plus optional `limit`, `page`, and `source` filters.

Design rule:

```text
Small provider-side adjacent APIs still need their own normalization layer; do not assume they can share the main chat bridge.
```

Track:

```text
anthropic_files_multipart_bridge
anthropic_files_openai_file_object_mapping
anthropic_files_purpose_default
anthropic_files_binary_content_passthrough
anthropic_skills_beta_header_merge
anthropic_skills_url_routing
anthropic_skills_query_filter_bridge
```

## Anthropic container uploads auto-inject a code-execution tool before the request leaves the proxy

Anthropic treats `container_upload` as a trigger, not as inert message
metadata. The chat transformer scans message content for that block type, and
when it finds one it appends a `code_execution_20250522` tool to the request
unless the caller already provided a code-execution tool. That injection is
what unlocks the container workflow on the Anthropic side, so the proxy has to
turn a message-side marker into an explicit tool declaration before dispatch.

Design rule:

```text
Some provider workflows are activated by content markers and must be converted into explicit tools upstream.
```

Track:

```text
anthropic_container_upload_triggers_code_execution
anthropic_code_execution_tool_auto_injection
anthropic_code_execution_tool_dedup
anthropic_container_upload_request_marker
```
