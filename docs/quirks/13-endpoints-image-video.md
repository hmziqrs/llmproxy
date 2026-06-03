# Mini Quirks — Endpoints: Image & Video

## Image generation has model-dependent endpoints

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
bedrock_nova_canvas_model_cost_resolution
```

## Gemini video generation is long-running and size-aware

Gemini Veo video generation is not a one-shot response. It starts with
`predictLongRunning`, then polls an operation until completion, then fetches
the generated video via the file API. The adapter also maps OpenAI-style
`size` values into Gemini `aspectRatio` and, when the edge size matches a
supported preset, a concrete `resolution` such as `720p` or `1080p`. It
normalizes `seconds` into `durationSeconds`, defaulting to 4 when the caller
omits it.

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
gemini_video_duration_seconds_default
```

## Gemini Interactions folds MIME and image config into response_format

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

## OpenAI video IDs and query variants are rewritten defensively

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

## Black Forest Labs image generation maps OpenAI knobs into model-specific controls

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

## Azure image edit resolves auth with Azure-style header precedence

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

## Fireworks image input gets a `#transform=inline` URL rewrite for non-vision models

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

## Recraft image edit collapses multi-image input to a single file

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

## DashScope image generation rewrites both prompt nesting and size syntax

DashScope's image-generation bridge does not accept the OpenAI request body
verbatim. The adapter nests the prompt under
`input.messages[0].content[0].text`, moves optional parameters into a
`parameters` object, and rewrites OpenAI `size` values from `WxH` into
DashScope's `W*H` syntax. On the response side, it pulls image URLs out of
`output.choices[0].message.content[*].image`, and it also treats API-level
errors embedded in a `200` response body as failures.

Design rule:

```text
Multimodal generators often need both request-body reshaping and response
envelope repair, including handling error payloads that still arrive with 200.
```

Track:

```text
dashscope_image_prompt_nesting
dashscope_size_wxh_to_wxh_star
dashscope_image_output_choice_unwrap
dashscope_api_level_error_in_200
```

## FAL AI image generation is a model-family switchboard with per-model size, format, and response-shape hacks

FAL AI is not a single image adapter. The dispatcher chooses a config by
substring, so `imagen4`, `recraft`, `bria`, `flux-pro`, `schnell`,
`bytedance/seedream`, `bytedance/dreamina`, `ideogram`, and `stable-diffusion`
all get different normalization rules. The base adapter also uses
`Authorization: Key ...` against `https://fal.run`, accepts response `images`
entries as either dicts or bare URL strings, and each model family rewrites
OpenAI `size` and `response_format` differently. Bria is also a shape outlier:
it returns a singular `image` object, while the other FAL adapters expect an
`images` list. Several families preserve provider metadata like `seed`,
`timings`, and `has_nsfw_concepts` in `_hidden_params`.

Design rule:

```text
If a provider family fans out into many model-specific request/response rules,
the family dispatcher itself is part of the protocol normalization surface.
```

Track:

```text
fal_ai_model_substring_dispatch
fal_ai_key_auth_header
fal_ai_flattened_image_response
fal_ai_bria_singular_image_shape
fal_ai_response_format_to_output_format
fal_ai_model_specific_size_mapping
fal_ai_hidden_generation_metadata
```

## AIML image generation rewrites route, size, format, and response envelopes

AIML’s image-generation adapter is a protocol bridge rather than a thin
OpenAI passthrough. It strips a trailing `/v1` from the base URL before
appending `/v1/images/generations`, accepts both `AIML_API_KEY` and
`AIMLAPI_KEY`, rewrites OpenAI `n` into `num_images`, `response_format` into
`output_format`, and maps `size` into `image_size` with either explicit
dimensions or a provider preset. The response side is also multi-shaped: it
accepts OpenAI-like `data[]`, `output.choices[]`, or raw `images[]` envelopes
and normalizes URLs, base64 payloads, and `revised_prompt` fields into a
single `ImageResponse`.

Design rule:

```text
Image adapters often need to normalize both the request route and the
provider’s multiple response envelopes, not just the payload fields.
```

Track:

```text
aiml_image_generation_route_normalization
aiml_image_generation_api_key_aliases
aiml_image_generation_n_to_num_images
aiml_image_generation_response_format_to_output_format
aiml_image_generation_size_to_image_size
aiml_image_generation_multi_envelope_response
```

## Cohere embeddings switch between text and image inputs and compute usage from billed units

Cohere embeddings are not a plain OpenAI `/v1/embeddings` passthrough. The
adapter maps OpenAI `encoding_format` into Cohere `embedding_types` and
`dimensions` into `output_dimension`. It then inspects the input strings: if
they look base64-encoded, it sends `images` with `input_type="image"`; otherwise
it sends `texts` with Cohere’s default embedding input type. On the response
side it consumes Cohere’s `embeddings` object, flattens each returned embedding
list into OpenAI-style items, and derives usage from `meta.billed_units`,
preferring provider-reported text/image token counts over local estimation when
available.

Design rule:

```text
Embedding adapters can switch request schema by modality and should trust provider-billed
usage when the API reports it.
```

Track:

```text
cohere_embedding_encoding_format_to_embedding_types
cohere_embedding_dimensions_to_output_dimension
cohere_embedding_base64_image_detection
cohere_embedding_texts_vs_images_request_shape
cohere_embedding_billed_units_usage
cohere_embedding_response_flattening
```

## Azure AI Cohere embeddings split base64 image inputs from text inputs and rewrite usage from response headers

Azure AI Cohere embeddings are not a single request path. The adapter scans the
input list and sends any base64-encoded items to the image-embedding route,
while keeping the remaining strings on the normal `/v1/embeddings` path. That
means one OpenAI request can become two provider requests, with the image
indices remembered for recombination. On the response side it reads usage from
Azure response headers, specifically `llm_provider-num_tokens`, and rewrites the
reported model from `llm_provider-azureml-model-group` back into the expected
base model name.

Design rule:

```text
Mixed-modality embedding requests need request splitting plus header-derived usage/model metadata.
```

Track:

```text
azure_ai_cohere_image_input_split
azure_ai_cohere_text_input_routing
azure_ai_cohere_image_embedding_recombination
azure_ai_cohere_header_usage_override
azure_ai_cohere_header_model_remap
```

## Azure AI Foundry FLUX image edit rewrites auth, deployment routing, and api-version handling

Azure AI Foundry FLUX image edit is not a direct OpenAI upload. The adapter
switches authentication to `Api-Key`, resolves the Azure AI base URL through
the Foundry helper, and requires an explicit API version. On the URL side it
either appends `/images/edits` to an existing deployment route or constructs
`/openai/deployments/{model}/images/edits` when the deployment name is only
available as the model. The final request URL always carries `api-version` as a
query parameter.

Design rule:

```text
Foundry image-edit endpoints need deployment-aware URL construction plus provider-specific auth headers.
```

Track:

```text
azure_ai_foundry_flux_image_edit_api_key_header
azure_ai_foundry_flux_image_edit_deployment_route
azure_ai_foundry_flux_image_edit_requires_api_version
azure_ai_foundry_flux_image_edit_query_api_version
```
