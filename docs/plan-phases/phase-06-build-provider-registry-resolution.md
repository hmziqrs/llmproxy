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
```

`llm-proxy-provider` can convert `ProviderAdapterTargetConfig` into its own
`ProviderAdapterTarget` by parsing `protocol`.

### Lookup rule

```text
requested_model -> ModelRoute
ModelRoute.upstream_model.unwrap_or(requested_model) -> upstream_model
provider.models[upstream_model] -> adapter_name
provider.adapters[adapter_name] -> protocol + endpoint
```

This lookup allows aliases:

```toml
[models]
"claude-4" = { provider = "opencode-zen", upstream_model = "claude-sonnet-4-20250514" }

[provider.models]
"claude-sonnet-4-20250514" = { adapter = "anthropic" }
```

### Tests

- provider registry loads multiple files
- duplicate provider names fail
- requested model alias resolves through upstream model
- provider-local missing model fails
- provider-local missing adapter fails
- protocol validation fails for unknown protocol

### Gate

```sh
cargo test -p llm-proxy-core provider_registry model_route
cargo test --workspace
```
