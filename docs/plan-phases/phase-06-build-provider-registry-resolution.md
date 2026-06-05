# Phase 6 - Build Provider Registry Resolution

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: connect TOML provider config to compiled provider adapters.

### Files

Add:

```text
crates/llm-proxy-core/src/provider_registry.rs
```

or keep this in `provider_config.rs` if it stays small.

Update:

```text
crates/llm-proxy-core/src/lib.rs
```

### Registry responsibilities

The registry loads provider TOML files and resolves a route target into a
provider adapter target.

Routing and registry resolution are separate:

```text
resolve_model_route(...) -> ProviderTarget
ProviderRegistry::resolve_adapter_target(ProviderTarget) -> ProviderAdapterTargetConfig
```

The router/model-route code selects `provider + requested_model +
upstream_model`. The provider registry only resolves that provider-local
upstream model to adapter config.

### Out of scope / guardrails

The provider registry must not:

- translate protocol fields
- parse response or stream chunks
- build final protocol-specific URL paths
- inspect messages or content
- mutate sampling, tools, metadata, reasoning, cache, or stream intent
- infer protocol families from model names

```rust
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: std::collections::HashMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    pub fn load_from_dir(path: impl AsRef<std::path::Path>) -> Result<Self, CoreError>;

    pub fn validate_protocols(
        &self,
        known_protocols: impl IntoIterator<Item = String>,
    ) -> Result<(), CoreError>;

    pub fn resolve_adapter_target(
        &self,
        target: &ProviderTarget,
    ) -> Result<ProviderAdapterTargetConfig, CoreError>;
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: std::collections::HashMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    pub fn load_from_dir(path: impl AsRef<std::path::Path>) -> Result<Self, CoreError>;

    pub fn validate_protocols(
        &self,
        known_protocols: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<(), CoreError>;

    pub fn resolve_adapter_target(
        &self,
        target: &ProviderTarget,
    ) -> Result<ProviderAdapterTargetConfig, CoreError>;
}

/// Manual `Debug` impl redacts `api_key` to `[REDACTED]` so that
/// `format!("{:?}", target)` never leaks credentials in logs or errors.
#[derive(Clone)]
pub struct ProviderAdapterTargetConfig {
    pub provider_name: String,
    pub adapter_name: String,
    pub protocol: String,
    pub endpoint: String,
    pub auth_style: AuthStyle,
    pub api_key: String,
    pub requested_model: String,
    pub upstream_model: String,
}

impl std::fmt::Debug for ProviderAdapterTargetConfig { /* redacts api_key */ }
```

`llm-proxy-provider` can convert `ProviderAdapterTargetConfig` into its own
`ProviderAdapterTarget` by parsing `protocol`.

`endpoint` is the raw endpoint or URL template from provider TOML. It is not a
route-built final URL. Provider adapters own endpoint URL shape, including
Gemini `{model}` expansion.

### Lookup rule

Router lookup:

```text
requested_model -> ModelRoute
ModelRoute.upstream_model.unwrap_or(requested_model) -> ProviderTarget.upstream_model
```

Registry lookup:

```text
ProviderTarget.provider -> provider
provider.models[ProviderTarget.upstream_model] -> adapter_name
provider.adapters[adapter_name] -> protocol + endpoint
```

This lookup allows aliases:

```toml
[models]
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }

[provider.models]
"claude-sonnet-4-20250514" = { adapter = "anthropic" }
```

### Error type

`CoreError` here is `llm_proxy_core::CoreError` — the core crate's config/registry error — NOT the protocol crate's stream error (`llm_proxy_protocol::core::CoreStreamError`). Registry resolution failures (duplicate provider name, missing provider, missing provider-local model, missing adapter, unknown protocol) are represented as `CoreError`: either reuse `CoreError::ConfigValidation { message }` with an actionable message, or add dedicated variants (e.g. `ProviderResolution`). Pick one and keep it consistent; the existing `CoreError` only has config-loading/parse/validation variants today, so new resolution errors must map onto `ConfigValidation` or new variants rather than a generic string.

### Tests

- provider registry loads multiple files
- duplicate provider names fail
- missing provider fails
- requested model alias resolves through upstream model
- `upstream_model` defaults to `requested_model`
- resolved target preserves both requested and upstream model names
- provider-local missing model fails
- provider-local missing adapter fails
- protocol validation fails for unknown protocol
- model names do not imply protocols; a Claude-looking model can resolve to an
  OpenAI adapter when TOML says so
- one provider with multiple adapters resolves only by provider-local model table
- resolution errors carry actionable messages (which provider/model/adapter/protocol failed)

### Gate

```sh
cargo test -p llm-proxy-core -- provider_registry
cargo test -p llm-proxy-core -- model_route
cargo test --workspace
```

Server/provider composition must also validate provider TOML against the
compiled `llm-proxy-provider` adapter registry by passing
`ProviderAdapterRegistry::protocol_names()`. Do not pass an ad hoc string list;
config cannot drift from actual protocol support.
