# Phase 3 - Add Provider Config And Routing Types

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: add TOML provider/model routing alongside the old JSON config. Do not
remove old `Config` yet.

### Files

Add:

```text
crates/llm-proxy-core/src/provider_config.rs
crates/llm-proxy-core/src/model_route.rs
```

Update:

```text
crates/llm-proxy-core/src/lib.rs
crates/llm-proxy-core/Cargo.toml
```

`toml` and `humantime-serde` already exist in workspace/core dependencies.

### Target TOML schema

Main config:

```toml
[server]
bind = "127.0.0.1:3456"
request_timeout = "300s"
log_level = "info"
hot_reload = false
server_name = "llm-proxy"

[models]
"kimi-k2.6" = { provider = "opencode-go" }
"glm-5" = { provider = "opencode-go" }
"gpt-5.4" = { provider = "opencode-zen" }
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }
```

Provider config:

```toml
[provider]
name = "opencode-zen"
api_key = "${OC_GO_CC_API_KEY}"
auth_style = "bearer"

[provider.adapters.responses]
protocol = "openai_responses"
endpoint = "https://opencode.ai/zen/v1/responses"

[provider.adapters.anthropic]
protocol = "anthropic_messages"
endpoint = "https://opencode.ai/zen/v1/messages"

[provider.adapters.gemini]
protocol = "gemini_generate_content"
endpoint = "https://opencode.ai/zen/v1/models/{model}:generateContent"

[provider.models]
"gpt-5.4" = { adapter = "responses" }
"claude-sonnet-4-20250514" = { adapter = "anthropic" }
"gemini-3.5-flash" = { adapter = "gemini" }
```

### Provider config examples

Concrete TOML examples live in
[`phase-03-provider-config-examples.md`](phase-03-provider-config-examples.md).
Keep this phase focused on the config types, validation, and routing behavior.

### Types

In `provider_config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub models: std::collections::HashMap<String, ModelRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: std::net::SocketAddr,
    #[serde(with = "humantime_serde")]
    pub request_timeout: std::time::Duration,
    pub log_level: String,
    pub hot_reload: bool,
    pub server_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFile {
    pub provider: ProviderConfig,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub name: String,
    pub api_key: String,
    pub auth_style: AuthStyle,
    pub adapters: std::collections::HashMap<String, ProviderAdapterConfig>,
    pub models: std::collections::HashMap<String, ProviderModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    Bearer,
    XApiKey,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAdapterConfig {
    pub protocol: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelConfig {
    pub adapter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    pub provider: String,
    pub upstream_model: Option<String>,
}
```

`ProviderConfig` needs a custom `Debug` implementation or redacted API-key
wrapper so resolved secrets do not appear in logs, snapshots, or test failure
output.

In `model_route.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTarget {
    pub provider: String,
    pub requested_model: String,
    pub upstream_model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelRouteError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
}

pub fn resolve_model_route(
    routes: &std::collections::HashMap<String, ModelRoute>,
    requested_model: &str,
) -> Result<ProviderTarget, ModelRouteError> {
    let route = routes
        .get(requested_model)
        .ok_or_else(|| ModelRouteError::UnknownModel(requested_model.to_owned()))?;

    Ok(ProviderTarget {
        provider: route.provider.clone(),
        requested_model: requested_model.to_owned(),
        upstream_model: route
            .upstream_model
            .clone()
            .unwrap_or_else(|| requested_model.to_owned()),
    })
}
```

Export the new config/routing types from `llm-proxy-core/src/lib.rs`:

```rust
pub use provider_config::{
    AppConfig, AuthStyle, ModelRoute, ProviderAdapterConfig, ProviderConfig,
    ProviderFile, ProviderModelConfig, ServerConfig,
};
pub use model_route::{ModelRouteError, ProviderTarget, resolve_model_route};
```

### Validation

Phase 3 validation checks config shape and same-phase references only:

- every provider-local model points to an existing adapter
- every endpoint is non-empty
- every `${ENV_VAR}` in `api_key` resolves to a non-empty value
- no `ModelRoute` contains endpoint/protocol fields
- provider names, route keys, adapter names, provider-local model keys,
  protocol names, and endpoints are non-empty
- literal `api_key` values are non-empty
- empty environment-variable values fail validation

Use `#[serde(deny_unknown_fields)]` so misplaced `endpoint` or `protocol`
fields inside `[models]` or `[provider.models]` fail while parsing instead of
being silently ignored.

Cross-file validation that needs both the main config and provider files moves
to Phase 6:

- every `[models]` provider exists
- every effective upstream model exists in the selected provider's
  `[provider.models]`
- every provider protocol name is implemented by the compiled adapter registry

Do not make `llm-proxy-core` depend on `llm-proxy-provider`. Core may expose a
validation function that accepts a caller-provided list of known protocol names,
but the compiled adapter registry lives in the provider/server composition
layer. The ownership split is:

```text
llm-proxy-core      -> parses TOML, resolves env vars, validates references
llm-proxy-provider  -> owns compiled provider protocol enum/adapters
llm-proxy-server    -> passes provider registry protocol names into core validation
```

Provider protocol validation must therefore happen when the server composes
`ProviderRegistry` with `ProviderAdapterRegistry::builtin()`, not while core is
parsing TOML in isolation.

### Tests

Add tests for:

- main TOML parse
- provider TOML parse
- `${ENV_VAR}` interpolation
- unknown env var fails validation
- empty env var fails validation
- literal empty API key fails validation
- unknown TOML fields fail parse
- `ModelRoute` with endpoint/protocol fields fails parse
- provider-local unknown adapter fails validation
- known protocol passes validation when supplied by caller
- unknown protocol fails validation when not supplied by caller
- unknown provider in route fails in Phase 6 cross-file registry validation
- effective upstream model missing from selected provider fails in Phase 6
- upstream model alias resolves correctly
- unknown model returns `ModelRouteError::UnknownModel`

### Gate

```sh
cargo test -p llm-proxy-core provider_config
cargo test -p llm-proxy-core model_route
cargo test --workspace
```
