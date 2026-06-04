# Phase 3 Provider Config Examples

These examples preserve the reusable provider-config detail from the earlier
audit. They belong here because Phase 3 is where the config shape becomes real.

#### Mixed OpenCode Go provider

One provider can expose several implemented provider protocols. The
provider-local `[provider.models]` table decides which adapter a resolved
upstream model uses.

```toml
[provider]
name = "opencode-go"
api_key = "${OC_GO_CC_API_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://opencode.ai/zen/go/v1/chat/completions"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/go/v1/messages"

[provider.models]
"kimi-k2.6" = { adapter = "chat" }
"glm-5" = { adapter = "chat" }
"glm-5.1" = { adapter = "chat" }
"qwen3.5-plus" = { adapter = "chat" }
"qwen3.6-plus" = { adapter = "chat" }
"qwen3.7-max" = { adapter = "chat" }
"deepseek-v4-pro" = { adapter = "chat" }
"deepseek-v4-flash" = { adapter = "chat" }
"minimax-m2.5" = { adapter = "anthropic" }
"minimax-m2.7" = { adapter = "anthropic" }
```

#### OpenAI Chat Completions-compatible provider

This is the common "TOML-only provider" case. It works without code changes
only because `openai_chat_completions` is already implemented.

```toml
[provider]
name = "chutes"
api_key = "${CHUTES_API_KEY}"
auth_style = "bearer"

[provider.adapters.chat]
protocol = "openai_chat_completions"
endpoint = "https://llm.chutes.ai/v1/chat/completions"

[provider.models]
"deepseek-v4" = { adapter = "chat" }
"llama-4" = { adapter = "chat" }
```

Main config route:

```toml
[models]
"deepseek-v4" = { provider = "chutes" }
"llama-4" = { provider = "chutes" }
```

#### Anthropic Messages-compatible provider

Use `auth_style = "both"` for providers that require both `x-api-key` and
`Authorization: Bearer ...`.

```toml
[provider]
name = "anthropic-compatible"
api_key = "${ANTHROPIC_COMPAT_API_KEY}"
auth_style = "both"

[provider.adapters.messages]
protocol = "anthropic_messages"
endpoint = "https://example.com/v1/messages"

[provider.models]
"claude-sonnet-4-20250514" = { adapter = "messages" }
```

Main config route with a client-facing alias:

```toml
[models]
"claude-4" = { provider = "anthropic-compatible", upstream_model = "claude-sonnet-4-20250514" }
```

#### OpenAI Responses-compatible provider

Responses support is a provider adapter, not a route special case.

```toml
[provider]
name = "responses-provider"
api_key = "${RESPONSES_PROVIDER_API_KEY}"
auth_style = "bearer"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://example.com/v1/responses"

[provider.models]
"gpt-5.4" = { adapter = "responses" }
"gpt-5.5" = { adapter = "responses" }
```

Main config route:

```toml
[models]
"gpt-5.4" = { provider = "responses-provider" }
"gpt-5.5" = { provider = "responses-provider" }
```

#### Gemini GenerateContent provider

URL templates are provider-adapter behavior. The router does not know that the
model appears in the Gemini URL path.

```toml
[provider]
name = "gemini-provider"
api_key = "${GEMINI_PROVIDER_API_KEY}"
auth_style = "bearer"

[provider.adapters.generate]
protocol = "gemini_generate_content"
endpoint = "https://example.com/v1/models/{model}:generateContent"

[provider.models]
"gemini-3.5-flash" = { adapter = "generate" }
```

Main config route:

```toml
[models]
"gemini-3.5-flash" = { provider = "gemini-provider" }
```

#### New provider wire protocol

If a provider does not speak one of the built-in protocol names, TOML is not
enough. Add code first:

```text
1. Add provider wire DTOs if the existing wire modules do not fit.
2. Add a provider adapter that converts CoreRequest into provider JSON.
3. Add response and stream decoders back into CoreResponse/CoreEvent.
4. Register the provider protocol name in ProviderAdapterRegistry::builtin().
5. Add provider golden fixtures in the same phase as the new adapter.
6. Add the provider TOML.
```

No client protocol adapter should change when adding a provider.

Back to Phase 3: [`phase-03-add-provider-config-and-routing-types.md`](phase-03-add-provider-config-and-routing-types.md).
Back to parent plan: [`docs/plan.md`](../plan.md).
