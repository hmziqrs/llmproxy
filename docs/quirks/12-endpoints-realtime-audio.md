# Mini Quirks — Endpoints: Realtime, Audio & TTS

## Gemini realtime remaps GA session fields back to beta keys

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

## Mistral transcription preserves extra fields in hidden params

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

## Azure Speech STT rewrites the base URL, sends a raw audio body, and extracts text from multiple response fields

Azure AI Speech transcription only accepts Cognitive Services or STT Speech
endpoints. The adapter rejects Azure OpenAI endpoints, normalizes the base URL
to the STT host, and maps OpenAI `response_format=verbose_json` to Azure's
`format=detailed` query value while building the request URL. The request body
is sent as raw audio bytes, not multipart form-data, with the processed file
content placed in `data` and the MIME type copied into `Content-Type`. On the
response side the adapter normalizes Azure Speech JSON into
`TranscriptionResponse(text=...)`, preferring `DisplayText`, then
`NBest[0].Display`, then `NBest[0].Lexical`, and preserving the full provider
payload in `_hidden_params`. It also hard-fails when `RecognitionStatus` is
present and not `Success`.

Design rule:

```text
Speech-to-text routing must validate the endpoint family, translate response-format names,
and handle provider-native raw-body / response-field conventions.
```

Track:

```text
azure_speech_stt_base_url_resolution
azure_speech_stt_reject_azure_openai_endpoint
azure_speech_stt_verbose_json_to_detailed
azure_speech_stt_raw_audio_body
azure_speech_stt_text_fallback_chain
azure_speech_stt_raw_response_preserved
```

## OpenAI Whisper forces verbose_json for duration-aware cost tracking

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

## OpenRouter Responses stays HTTP, not native WebSocket

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

## Realtime beta headers gate the event contract

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

## Bedrock Nova Sonic realtime has fixed audio sample-rate defaults

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

## WebSocket responses need the model injected into the URL

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

## NVIDIA Riva transcription is a gRPC bridge with its own endpointing model

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

## Gemini realtime drops unknown event types instead of forwarding them

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

## Azure realtime uses a two-step handshake instead of one live endpoint

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

## Gemini realtime buffers standalone usage metadata for the next response

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

## OpenAI realtime HTTP uses a bootstrap URL and a separate live-call URL

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

## xAI realtime speaks the OpenAI websocket shape without the beta header

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

## Deepgram transcription is a protocol bridge on both request and response

The Deepgram transcription adapter is not a pass-through. The request path
sends processed audio bytes directly as the body, converts the OpenAI
`language` param into the query string, and builds the Deepgram `/v1/listen`
URL with encoded query params. The response path pulls the transcript from the
first channel/alternative, chooses a diarized or plain transcript based on
speaker metadata, rebuilds speaker-labeled text when paragraphs are absent,
adds OpenAI-style fields like `task`, `language`, `duration`, and `words`, and
preserves the full provider JSON in `_hidden_params` for later inspection.

Design rule:

```text
Audio transcription adapters may need to translate both payload transport
(bytes versus multipart) and transcript assembly (flat versus diarized).
```

Track:

```text
deepgram_raw_audio_request_body
deepgram_language_query_param
deepgram_listen_endpoint_query_string
deepgram_transcript_from_first_channel
deepgram_diarized_text_reconstruction
deepgram_openai_field_backfill
deepgram_hidden_provider_json
```

## Hosted vLLM transcription appends the transcription path to the caller's base URL

The Hosted vLLM transcription adapter expects the caller to provide a base
URL and then normalizes it into the full
`/v1/audio/transcriptions` endpoint. It does not synthesize a default host, so
the upstream deployment location is still caller-owned, while the adapter owns
the final path shape.

Design rule:

```text
Transcription adapters may need a caller-provided host but still own the final
endpoint suffix.
```

Track:

```text
hosted_vllm_transcription_append_path
hosted_vllm_transcription_api_base_required
```

## MiniMax splits into separate chat and TTS contracts with different normalization rules

MiniMax is not one uniform OpenAI-compatible surface. The chat adapter keeps
`cache_control` intact, exposes `reasoning_split`, and conditionally exposes
`thinking` for reasoning-capable models. The TTS adapter is more opinionated:
it resolves OpenAI voice aliases into MiniMax `voice_id` values, falls back to
a default voice when none is provided, clamps `speed` into MiniMax’s supported
range, maps response formats into `format`, and forces HTTP output into a
binary decode path. URL output is treated as unsupported for now.

Design rule:

```text
When one provider exposes multiple product surfaces, each surface may need a
different compatibility contract even if the base host looks uniform.
```

Track:

```text
minimax_chat_cache_control_preserved
minimax_chat_reasoning_split
minimax_chat_thinking_gate
minimax_tts_voice_alias_map
minimax_tts_default_voice
minimax_tts_speed_clamp
minimax_tts_response_format_map
minimax_tts_url_output_unsupported
```

## AWS Polly and Azure AVA TTS both translate OpenAI voice/format/speed into provider-specific synthesis contracts

The text-to-speech adapters are not pass-throughs. AWS Polly resolves voice
aliases into Polly voices, maps response formats into Polly output formats,
derives the engine from the model name, and forwards Polly-specific request
keys like `LanguageCode`, `LexiconNames`, and `SampleRate`. Azure AVA does a
different translation: it maps OpenAI voices into Azure neural voices, turns
response formats into Azure audio content types, converts `speed` into SSML
`prosody` rate, wraps content in `mstts:express-as` when style attributes
are present, and treats input that already looks like SSML as a pass-through
body instead of wrapping it again. Both paths are doing real
synthesis-protocol translation rather than simple header setup.

Design rule:

```text
TTS adapters often have to bridge voice names, output codecs, and SSML
features, not just route to a provider endpoint.
```

Track:

```text
aws_polly_voice_alias_map
aws_polly_response_format_map
aws_polly_engine_from_model
aws_polly_ssml_detection
azure_tts_voice_alias_map
azure_tts_output_format_map
azure_tts_speed_to_rate
azure_tts_express_as_wrapper
azure_tts_ssml_passthrough
```

## ElevenLabs transcription and TTS both translate OpenAI fields into provider-specific file and URL contracts

ElevenLabs is not a plain OpenAI audio backend. The transcription adapter
rewrites `language` into `language_code`, sends multipart form-data with the
audio file, and preserves the provider response in hidden params while
collapsing only `word` items into OpenAI-style words. The TTS adapter is even
more specific: it requires a voice ID, maps OpenAI voice aliases to ElevenLabs
voice IDs, turns response formats into query parameters, encodes the voice ID
into the URL path, and stores extra provider/query parameters separately so
the request can be reconstructed at dispatch time.

Design rule:

```text
Audio adapters often need to split transport state between body, path, query,
and hidden request metadata to match the provider’s contract.
```

Track:

```text
elevenlabs_language_to_language_code
elevenlabs_multipart_audio_request
elevenlabs_hidden_transcription_json
elevenlabs_voice_alias_map
elevenlabs_voice_id_path_encoding
elevenlabs_tts_query_params_split
elevenlabs_required_voice_id
```

## Scaleway transcription accepts multipart uploads and can fall back to raw text when the response is not JSON

Scaleway’s transcription adapter is OpenAI-shaped on the request side, but it
still has provider-specific response handling. It uploads the audio as
multipart form-data with `file` plus `model`, carries OpenAI transcription
fields through as form fields, and on the response side it checks
`content-type`: if the response is not JSON, it returns the raw text directly
instead of failing. When JSON is present, it preserves `segments` and
`language` in the transcription response and stores the full provider payload
in hidden params.

Design rule:

```text
Transcription adapters sometimes need to handle both structured JSON and
plain-text fallback responses from the same endpoint.
```

Track:

```text
scaleway_transcription_multipart_upload
scaleway_transcription_raw_text_fallback
scaleway_transcription_segments_preserved
scaleway_transcription_language_preserved
scaleway_hidden_provider_json
```

## Bedrock TwelveLabs Marengo switches request shape by input type and wraps async jobs as hidden invocation ARNs

The TwelveLabs Marengo embedding adapter does real envelope translation. It maps
OpenAI-style `encoding_format=float` into `embeddingOption=["visual-text",
"visual-image"]`, renames `input_type` to `inputType`, defaults
`textTruncate` to `end` for text inputs, and routes image/video/audio inputs
through either inline base64 or `s3://` media sources. Video and audio require
the async-invoke path, where the adapter strips `async_invoke/` from the model
id, requires a non-empty `output_s3_uri`, and wraps the request in Bedrock’s
`modelId` / `modelInput` / `outputDataConfig` envelope. On the response side it
normalizes three different payload shapes (`data`, direct `embedding`, and
`embeddings`), reindexes embeddings sequentially, estimates usage from
`inputTextTokenCount` when present, and stores the async `invocationArn` in
hidden params.

Design rule:

```text
Multimodal embedding adapters need to branch on input type, async invocation,
and provider-specific job handles, not just on the model name.
```

Track:

```text
twelvelabs_marengo_encoding_format_to_embedding_option
twelvelabs_marengo_input_type_to_inputType
twelvelabs_marengo_texttruncate_default_end
twelvelabs_marengo_s3_or_base64_media_source
twelvelabs_marengo_async_invoke_model_wrap
twelvelabs_marengo_sequential_embedding_reindex
twelvelabs_marengo_hidden_invocation_arn
```

## Azure Document Intelligence OCR normalizes page selection, polls async operations, and rebuilds Mistral-style pages

Azure Document Intelligence OCR is an async document-analysis bridge, not a
plain OCR call. The adapter translates Mistral-style `pages` into Azure’s
1-based query string, accepting either 0-based integer lists or Azure-native
page strings. It also chooses between `base64Source` and `urlSource` based on
the input document type. On the response side it handles Azure’s 202
`Operation-Location` flow by polling until the analysis status becomes
`succeeded`, rejecting cross-origin polling URLs along the way. Once complete,
it rebuilds the response into Mistral-shaped pages by converting each Azure
page to a 0-based index, extracting markdown from the `lines` content, and
converting inches into pixels for page dimensions.

Design rule:

```text
OCR adapters need both request-shape normalization and a long-running async operation bridge.
```

Track:

```text
azure_document_intelligence_pages_query_normalization
azure_document_intelligence_base64_or_url_source
azure_document_intelligence_202_operation_polling
azure_document_intelligence_cross_origin_polling_reject
azure_document_intelligence_page_index_and_markdown_rebuild
azure_document_intelligence_dimension_conversion
```

## OpenAI audio-transcription guardrails are output-only

OpenAI audio-transcription guardrails are output-only: the input path is a
no-op because the payload is binary audio, while the transcribed text is
re-guardrailed after transcription. The handler also injects request metadata
into `request_data` so the guardrail layer can see the response context.

Design rule:

```text
Guardrails on binary-input endpoints must run on the decoded output, not the request payload.
```

Track:

```text
openai_audio_transcription_output_only_guardrails
openai_audio_transcription_request_metadata_injection
```
