# Quirks

A flat catalogue of gotchas, footguns, surprises, and broken things
found in the four reference projects. Observations only. No
recommendations. No connection to our design.

## `ref/oc-go-cc` (Go)

### Bugs and broken behavior

- **Stream fast path assumes a fixed key substring.**
  `internal/transformer/stream.go:239-243` short-circuits the JSON
  parser by checking for the absence of `"reasoning_content"`,
  `"finish_reason"`, `"tool_calls"`, and `"usage"`. If an upstream
  adds a new field to a content chunk, the chunk silently falls
  through to the slow path with no warning. If the substring it
  *does* look for — `"delta":{"content":"` — ever changes order
  or quoting, the fast path returns the wrong content.
- **`replaceModelInRawBody` mutates JSON by string surgery.**
  `internal/handlers/messages.go:447-466` finds `"model":"` in the
  raw bytes and replaces it. If a request body has the literal
  string `"model":"` somewhere it isn't a top-level field (e.g.
  inside a tool's `input` schema), the surgery picks the first
  match and corrupts the body. The function logs a warning and
  returns the original if it can't find the field, but a wrong
  first match is silent.
- **`usageInfoToAnthropic` math can go negative.**
  `internal/transformer/stream.go:572` and
  `response.go:18-23` use a `nonNegative()` clamp on
  `prompt_tokens - cache_hit - cache_miss` because some
  upstreams report cache parts that don't sum cleanly. The
  result is that token counts silently round to zero in those
  cases — clients' context counters see a "free" turn.
- **MINIMAX is a placeholder string.**
  `internal/client/opencode.go:58` lists `"minimax-m2.5"`,
  `"minimax-m2.7"`, `"qwen3.7-max"`. Not a real provider name.
  Either dead code or an unfinished rename. The Anthropic
  endpoint branch in the handler routes to it anyway.
- **`rate_limiter` map has no eviction.**
  `internal/middleware/middleware.go:97-143` uses a plain
  `map[string]*clientTokenBucket` with no expiry. Under any
  sustained traffic from rotating IPs the map grows without
  bound.
- **Stream heartbeat goroutine and the request goroutine are
  not joined.** `internal/handlers/messages.go:242-266` launches
  a ticker that watches `atomic.Int32` `finished`. The
  `defer close(heartbeatDone)` is correct in normal flow, but
  if `rw.WriteHeader` panics inside the streaming loop the
  goroutine can leak until the next ticker tick.
- **`requestDedup` cancels a fresh request if the previous
  finishes within 500ms of the new one's arrival.** The dedup
  window is hardcoded at `internal/middleware/middleware.go:27`.
  Legitimate repeated-but-uncorrelated requests within 500ms
  are dropped.

### Naming and copy-paste

- Package name `oc-go-cc`, binary name `oc-go-cc`, repo name
  `oc-go-cc`. The config dir is `~/.config/oc-go-cc/`. The Go
  module path is just `oc-go-cc`. Some user-facing strings
  hyphenate, some don't.
- The model ID list in `cmd/oc-go-cc/main.go:285-307` is
  hardcoded in two places: the `models` command and the
  default config template. They are not auto-derived from
  each other.
- `internal/router/router.go` is a 2-line placeholder file
  with `package router`. The real router lives in
  `model_router.go` and `scenarios.go` in the same package.

### Surprising choices

- Routing is config-driven except for `IsAnthropicModel()`
  (`internal/client/opencode.go:56-72`), which is hardcoded.
  Adding a new model with a different endpoint requires code.
  This asymmetry is not documented in `CONFIGURATION.md`.
- `ScenarioBackground` returns false if **any** tool keyword
  appears anywhere in the message content
  (`internal/router/scenarios.go:146-180`). A user asking
  "what does this tool do?" trips the blocker and the request
  is routed as default, not background.
- Hot-reload config callback for the CLI `--port` override is
  registered at `cmd/oc-go-cc/main.go:124-128` so the override
  survives reloads. The host, by contrast, is not preserved —
  the port persists, the host reverts to config.
- `TIKTOKEN_CACHE_DIR` is set via `os.Setenv` at counter init
  (`internal/token/counter.go:38`). The set is a side effect of
  construction. If the counter is ever constructed twice, the
  second call still wins; if it's never constructed, the env
  var is unset.

## `ref/llm-api-key-proxy` (Python, FastAPI)

### Bugs and broken behavior

- **`verify_api_key` allows open access when the env var is
  empty.** `src/proxy_app/main.py:666-678` returns immediately
  if `PROXY_API_KEY` is unset. Default in the bundled `.env`
  is empty. Production deployments that forget to set the
  env var are silently public.
- **CORS is `allow_origins=["*"]` with
  `allow_credentials=True`.** `src/proxy_app/main.py:646-652`.
  Browsers reject this combination, but a custom client can
  still exploit it.
- **Embedding batcher assumes a single model per batch.**
  `src/proxy_app/batch_manager.py:9-84` runs one worker. A
  batch mixing `text-embedding-3-small` and
  `text-embedding-3-large` will serialize through the same
  model call (the second is ignored or misrouted — the code
  reads "assumes same model across the batch" in a comment).
- **`force_timeout` defaults to True, but the wired
  implementation is the broken thread-based one.**
  `src/rotator_library/config/defaults.py:175-176` plus
  `src/rotator_library/utils/timeout_function.py:6-30` plus
  `src/rotator_library/providers/google/vertexai.py:294-300`.
  The thread cannot be killed. The comment in
  `llmproxy.config.yml:6` says "WARNING: This can cause
  additional costs!" — but this is the default.
- **`_try_fair_cycle_reset` can recurse without bound.**
  `src/rotator_library/usage/selection/engine.py:111-131`.
  No max depth in the recursion. Theoretical infinite loop.
- **`asyncio.Lock` in `UsageManager` serializes all writes.**
  `src/rotator_library/usage/manager.py:64` — single lock for
  the entire tracking engine, not per-provider. Bottleneck
  at any non-trivial QPS.
- **`ResilientStateWriter` debounce is 5s.** A crash within
  5s of a state change loses the change.
  `src/rotator_library/usage/persistence/storage.py:83`.
- **`transaction_logger.py` writes per-request to disk on the
  hot path.** `src/rotator_library/transaction_logger.py:94`.
  Creates a directory per request under
  `logs/transactions/MMDD_HHMMSS_{provider}_{model}_{reqid}/`.
  Disk-bound at high QPS.
- **`mask_credential` shows file basenames.** Leaks the
  directory structure of where the credential file lives.
  `src/rotator_library/error_handler.py:250-296`.

### Surprising choices

- **The codebase has no internal canonical request type.**
  OpenAI Chat Completions is the implicit canonical wire.
  Anthropic is translated to OpenAI on the way in
  (`src/rotator_library/anthropic_compat/translator.py`) and
  back on the way out. The translation lives in
  `anthropic_compat/streaming.py` (433 LOC) and
  `anthropic_compat/translator.py` (629 LOC).
- **Tool-call argument reassembly is duplicated.**
  `src/proxy_app/main.py:697-859` parses each `data:` chunk,
  aggregates `tool_calls[i].function.arguments` per index, and
  reassembles a `{object: "chat.completion", choices: [...]}` —
  for the transaction log only. The streamed bytes are passed
  through unchanged. So the request is parsed twice.
- **`RequestExecutor._execute_non_streaming` is 1495 lines.**
  Single function with both credential rotation and the full
  retry policy. `src/rotator_library/client/executor.py:477`.
- **`RequestExecutor._execute_streaming` is 446 lines.**
  Same pattern: 720-1166 in the same file.
- **`gemini_cli_provider.py` is 2039 lines for one provider.**
  Single file containing the Google Code Assist OAuth flow,
  tier detection via project metadata, and preview-model
  fallback order. `src/rotator_library/providers/gemini_cli_provider.py`.
- **LITELLM log level is set globally to ERROR**
  (`src/rotator_library/client/rotating_client.py:117-120`)
  and cannot be re-enabled per call.
- **`has_custom_logic` flag is checked per request** at
  `src/rotator_library/providers/provider_interface.py:281`.
  Compile-time knowledge is encoded as a runtime predicate.
- **`AnthropicMessagesRequest` lives in
  `anthropic_compat/models.py`** — one Pydantic model per
  direction. The OpenAI shape has no Pydantic model because
  it's implicit.
- **`OVERRIDE_TEMPERATURE_ZERO` silently mutates inbound
  requests** at `src/proxy_app/main.py:892-910`. Two modes
  (`remove` / `set`); behavior depends on env. No audit log
  of the mutation.

### Naming

- Directory is `rotator_library`. The package is published
  with a different name. README says "API Key Proxy" in some
  places, "RotatingClient" in others.
- `PROVIDER_PLUGINS.get(provider)` is the only lookup
  mechanism; the registry is a plain `dict` with no
  schema, no validation, and no introspection.

## `ref/llm-proxy` (proxyllm, Python SDK)

### Critical context

- **This is not an HTTP proxy.** It is an in-process Python
  SDK. `LLMProxy.route(prompt=...)` is a function call, not
  an HTTP route. There is no FastAPI, Flask, aiohttp,
  Starlette, uvicorn, or any ASGI/WSGI framework anywhere
  in the package. The "inbound" surface is the calling
  Python program. The "outbound" surface is each provider's
  SDK. This is the most important fact about this reference.

### Bugs and broken behavior

- **`prompt.replace(" ", "")` is the Vertex tokenize
  function.** `proxyllm/utils/tokenizer.py:35-36`. The
  accompanying comment: "Currently, this function simplifies
  the prompt by removing spaces, which is not a typical
  behavior for actual encoding."
- **Anthropic tokenize constructs a full `Anthropic(api_key)`
  client per call.** `proxyllm/provider/anthropic/claude.py:165-172`.
  Wasted client construction per tokenize call.
- **`timeout_function.timeout_wrapper` uses daemon threads
  that cannot be killed.** `proxyllm/utils/timeout_function.py:6-30`.
  The provider SDK call still incurs its full cost after the
  wrapper times out. The `multiprocessing` version
  (`timeout_function.py:66-99`) works correctly but is not
  the wired-in default.
- **`ModelType.CODEY.value = ["code-bison,codechat-bison,code-gecko"]`**
  is a list with a single comma-separated string.
  `proxyllm/provider/google/vertexai.py:91-94`. Almost
  certainly a bug — the other ModelType values are lists
  of separate model IDs.
- **The single BPE tokenizer is trained on a wiki dump.**
  `proxyllm/data/tokenizer-wiki.json` (~10K vocab) and
  `proxyllm/training/train_BPE.py`. Used for Cohere,
  Mistral, and Llama-2. Wildly inappropriate for code, math,
  and non-English.
- **Elo ratings are stale and explicitly marked as such.**
  `proxyllm/config/internal_config.py:216`: `"command-nightly":
  ..., "elo": 500,  # estimated, latest but unstable`. Other
  Elo values are LMSYS-Chatbot-Arena snapshots from some
  point in 2024.

### Surprising choices

- **`transformers` + `torch` are runtime dependencies.**
  `pyproject.toml:15-16` declares them. They're pulled in
  only for `categorize_text()` at
  `proxyllm/utils/categorization.py:1-39` which loads
  `facebook/bart-large-mnli`. A server-side proxy that needs
  to pick a model category is now ~2GB heavier than it
  needs to be.
- **Cost is an upper-bound estimate, not actual.**
  `proxyllm/utils/cost.py:1-38` always uses
  `max_output_tokens` for the output-cost leg. A model that
  emits 50 tokens is costed as if it emitted 4096. This
  biases routing toward cheap-output-per-token models.
- **Per-provider tokenization is uneven.** OpenAI uses
  tiktoken, Anthropic uses the SDK (one client per call),
  Vertex uses `replace(" ", "")`, and Cohere/Mistral/Llama-2
  share the wiki BPE.
- **Adapter discovery is by string path.**
  `proxyllm/proxyllm.py:106-115` does
  `importlib.import_module` + `getattr` based on
  `"adapter_path": "proxyllm.provider.openai.chatgpt.OpenAIAdapter"`
  in `internal_config.py`. No entry points, no plugin
  metadata.
- **HuggingFace Llama-2 ignores `chat_history` entirely.**
  `proxyllm/provider/huggingface/llama2.py:233-249` inlines
  the prompt in Llama-2's chat template and discards the
  history. The history is still appended to the returned
  object for symmetry, so the caller doesn't know.
- **Category routing is "shifted away from."** Comment at
  `tests/unit_tests/utils/test_categorization.py:1-11`:
  "Ommited to speed up unit tests, as cateogry routing
  focus is shifted to effectiveness/proficency routing".
  The code path is intact but deprioritized.
- **`CompletionResponse` has no usage, no finish_reason, no
  tool calls.** `proxyllm/proxyllm.py:25-40`. The only fields
  are `response: str`, `response_model: str`, `errors: list`,
  `chat_history: list`. Callers can't budget tokens or
  detect refusals.

### Naming

- Package is `proxyllm` (lowercase, no hyphen) per
  `pyproject.toml:2`. README sometimes says "LLM Proxy"
  (README.md:3), sometimes "ProxyLLM"
  (proxyllm/provider/anthropic/claude.py:56). The GitHub URL
  is `github.com/llm-proxy/llm-proxy`
  (proxyllm/cli.py:5).

## `ref/litellm` (Python framework)

### Scale

- 1,908 files, 624,910 LOC.
- `litellm/proxy/` is 226,932 LOC — the FastAPI server
  surface.
- `litellm/llms/` is 179,057 LOC across 121 provider dirs.
- `litellm/__init__.py` re-exports from many submodules and
  is itself large.
- `litellm/utils.py` is the central dispatch and is
  famously long.

### Bugs and broken behavior

- **`LlmProviders` is a string-typed enum of 100+ values** at
  `litellm/types/utils.py:3235`. Typos in the enum are caught
  only at runtime. There's no compile-time guarantee that a
  provider added to the enum has a corresponding config class.
- **`ProviderConfigManager._PROVIDER_CONFIG_MAP`** at
  `litellm/utils.py:8245` is a `dict[LlmProviders, tuple[Callable[[], BaseConfig], bool]]`
  — factory functions captured lazily. A factory that
  panics at construction will surface only when the dict is
  first accessed, not at registration.
- **`route_type` literal at
  `litellm/proxy/common_request_processing.py:753`** is a
  `match` with ~50 arms. No exhaustiveness check from the
  type system.
- **`BaseLLMHTTPHandler` is 12,747 LOC** at
  `litellm/llms/custom_httpx/llm_http_handler.py:183`. Single
  class owns HTTP transport, request signing, retries,
  timeout, response parsing dispatch, streaming dispatch,
  error mapping. Touching it is high-risk.
- **`CustomStreamWrapper` is 2,450 LOC** at
  `litellm/litellm_core_utils/streaming_handler.py:100`. Single
  class owns the OpenAI-shape SSE emitter.
- **`ModelResponse` is the OpenAI shape, not an
  IR.** `litellm/types/utils.py:1890`. Every non-OpenAI
  provider translates *to* this on the way out. Provider
  specifics that don't fit go into `_hidden_params` and
  `provider_specific_fields` — passthrough escape hatches
  with no validation.
- **`BaseConfig` has 11 overridable methods.** Per-provider
  overrides are scattered: `get_supported_openai_params`,
  `map_openai_params`, `transform_request`,
  `transform_response`, `transform_parsed_response`,
  `get_model_response_iterator`, `validate_environment`,
  `get_complete_url`, `get_error_class`,
  `get_required_params`, `get_optional_params`,
  `get_finished_stream_reason`. Forgetting any one of them
  in a new subclass is a silent gap.
- **`openai_compatible_providers`** at
  `litellm/constants.py:790` is a `list` (not a `dict`) of
  ~38 names. Used for membership checks; a typo in a new
  provider name is silent.
- **`litellm/llms/openai_like/providers.json`** has 19
  entries. JSON-loaded at runtime; no schema validation; a
  malformed entry fails on first request to that provider.
- **`create_config_class()`** at
  `litellm/llms/openai_like/dynamic_config.py:19` generates a
  `JSONProviderConfig(OpenAIGPTConfig)` subclass at runtime
  via meta-programming. Stack traces from a generated class
  are unfriendly.

### Surprising choices

- **Anthropic is not a first-class native; it goes through
  OpenAI as the hub.** The "spoke" pattern is documented in
  `litellm/llms/anthropic/experimental_pass_through/architecture.md`.
  Non-Anthropic upstreams serving Anthropic-format clients
  are routed through
  `translate_anthropic_to_openai` → `litellm.completion()` →
  `translate_openai_response_to_anthropic`. The translation
  is a pure-function module, not a class.
- **Three base-class hierarchies exist in parallel** for
  chat-completions (`BaseConfig`), responses API
  (`BaseResponsesAPIConfig`), and Anthropic messages
  (`BaseAnthropicMessagesConfig`). Plus a fourth:
  `CompletionTransformationBridge` for direct
  spec-to-spec bridges. A provider that wants to serve all
  three surfaces writes three classes.
- **`get_optional_params` is a two-pass design.** First it
  flattens the request using the provider's
  `map_openai_params`; then `transform_request` produces the
  wire body. The split between "which params are supported"
  and "how to format them" is what lets a single
  `BaseLLMHTTPHandler` reach any provider.
- **`get_llm_provider()`** at
  `litellm/litellm_core_utils/get_llm_provider_logic.py:137`
  parses the model string `anthropic/claude-3-5-sonnet` →
  `(model, custom_llm_provider, api_key, api_base)` once per
  request. The parse is the universal routing key.
- **A2A protocol support is 4,343 LOC** at
  `litellm/a2a_protocol/`. Distinct scope from the
  proxy core.
- **MCP support lives in `experimental_mcp_client/`** —
  literally marked experimental. The Anthropic-spec MCP
  protocol is supported as an inbound.
- **The LiteLLM Responses API handlers
  (`litellm/responses/`)** are a separate code path from
  the Chat Completions handlers. The two are bridged via
  `CompletionTransformationBridge` subclasses in
  `litellm/responses/litellm_completion_transformation/handler.py`.
- **`get_supported_openai_params` returns a list of param
  *names*** — a string-level contract between provider
  subclasses and the handler. No schema. A new OpenAI
  parameter must be added to every provider's list
  explicitly.

## Cross-cutting

Things that appear in more than one reference, suggesting they
are recurring traps rather than one-off mistakes.

### Tokenization is unreliable

- `ref/oc-go-cc/internal/token/counter.go` uses `tiktoken-go`
  (cl100k_base). Correct.
- `ref/llm-api-key-proxy` uses `litellm.token_counter` —
  depends on LiteLLM's per-model knowledge.
- `ref/llm-proxy` (proxyllm):
  - OpenAI: `tiktoken.encoding_for_model(self.model)`.
  - Anthropic: full SDK client per call.
  - Vertex: `prompt.replace(" ", "")`.
  - Cohere, Mistral, Llama-2: one wiki-trained BPE tokenizer.
- The Vertex tokenizer is a placeholder. The Llama-2
  tokenizer is the wrong corpus. The Anthropic one is
  inefficient. None of them agree on the same prompt.

### Tool-call argument reassembly is duplicated

- `ref/oc-go-cc/internal/transformer/stream.go:414-479`
  buffers `delta.tool_calls[i].function.arguments` per
  `i`, then emits Anthropic `input_json_delta` per
  Anthropic content-block index.
- `ref/llm-api-key-proxy/src/proxy_app/main.py:745-859`
  does the same aggregation in `streaming_response_wrapper`
  — but for the transaction log, not the wire output. The
  wire output is passthrough.
- `ref/litellm/litellm/litellm_core_utils/streaming_handler.py:1142`
  has its own `chunk_creator` that dispatches on chunk type
  for the OpenAI-shape output.
- Three different code paths, same logical operation, none
  sharing fixtures.

### Anthropic thinking blocks need signature preservation

- `ref/oc-go-cc/internal/transformer/stream.go:328-366`
  handles `reasoning_content` but does not preserve
  Anthropic's `signature` field across turns.
- `ref/llm-api-key-proxy/src/rotator_library/anthropic_compat/translator.py:108-114`
  strips everything from the thinking block except `type`,
  `thinking`, and optional `signature`. The comment
  explains: "only these keys are kept." The function
  `_budget_to_reasoning_effort` at `:40-79` maps Anthropic
  budget tokens to provider `reasoning_effort`.
- `ref/litellm/litellm/llms/anthropic/chat/handler.py:536`
  has its own `ModelResponseIterator` that has to handle
  the same translation.
- If `signature` is dropped on a turn, the next turn that
  replays the conversation can be rejected by Anthropic.

### Stop-reason mapping is non-trivial

The full set that has to be reconciled:

```
end_turn, stop, STOP, model_length, tool_use, function_call,
content_filter, safety, max_tokens, other
```

- `ref/oc-go-cc/internal/transformer/response.go:48` has
  `mapFinishReason` mapping OpenAI to Anthropic.
- `ref/llm-api-key-proxy/src/rotator_library/anthropic_compat/translator.py`
  has its own mapping.
- `ref/litellm/litellm/llms/base_llm/chat/transformation.py`
  has `get_finished_stream_reason` as one of the 11
  overridable methods.
- Three implementations, none shared.

### Usage token math is provider-specific

- Anthropic `input_tokens` = non-cached only.
- OpenAI `prompt_tokens` = total.
- Gemini `thoughtsTokenCount` is separate.
- `ref/oc-go-cc/internal/transformer/response.go:60-72`
  documents the trap: "Claude Code's local context counter
  sees an inflated input_tokens on every turn and trips
  auto-compact ~5x too early on long-prefix sessions" if
  the math is wrong.
- The `nonNegative()` clamp exists because the parts don't
  always sum cleanly. The result is silent zero.

### Hot-reload / config-change callbacks are easy to leak

- `ref/oc-go-cc/internal/config/atomic.go` registers
  callbacks via `OnReload`. The CLI port-override callback
  is registered at construction and is never removed.
- `ref/llm-api-key-proxy` has `background_refresher.py` —
  a single `asyncio.Task` per provider. An error in one
  task doesn't surface to others.
- `ref/litellm` has a config-reload system for the proxy
  server that re-creates the router on change. The exact
  state preserved across reloads is implicit.

### Logging is everywhere

- `ref/oc-go-cc/internal/server/server.go:36-39` sets a
  default `slog` logger at construction.
- `ref/llm-api-key-proxy` writes per-request dirs to disk
  on the hot path (`transaction_logger.py:94-633`).
- `ref/proxyllm/proxyllm/utils/proxy_logger.py:50-189` is
  a singleton with ANSI color + per-day log files.
- `ref/litellm/integrations/` is 47,276 LOC of logging,
  callback, and observability integrations.
- None of them share a logging convention.
