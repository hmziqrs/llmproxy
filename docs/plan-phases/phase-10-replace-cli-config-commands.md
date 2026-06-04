# Phase 10 - Replace CLI Config Commands

> Parent plan: [`docs/plan.md`](../plan.md)
> Standards: [`docs/protocol-mini.md`](../protocol-mini.md), [`docs/protocol-normalization.md`](../protocol-normalization.md)

Goal: CLI generates and validates the new TOML/provider config.

### Files

Update:

```text
apps/llm-proxy/src/main.rs
```

Add examples:

```text
config.toml.example
providers/opencode-go.toml.example
providers/opencode-zen.toml.example
```

or generate them directly from `llm-proxy init`.

### CLI changes

`serve`:

- `--config` points to `config.toml`.
- `$LLM_PROXY_CONFIG` is the preferred env var.
- `$OC_GO_CC_CONFIG` may be detected temporarily only to print the migration
  warning; do not use it as the preferred live config path.

`init`:

- creates `config.toml`
- creates `providers/opencode-go.toml`
- creates `providers/opencode-zen.toml`
- does not write fallback/scenario JSON

`validate`:

- loads main TOML
- loads provider files
- validates provider adapter protocols against builtins
- prints model route table:

  ```text
  client_model -> provider/upstream_model/adapter/protocol
  ```

`models`:

- lists configured client models from `[models]`
- optionally accepts `--provider` later, but not required in this phase

### Backward compatibility

Phase 10 is the config cutover point. Start this phase only after:

- `/v1/messages` uses the core pipeline
- `/v1/chat/completions` uses the core pipeline
- `/v1/messages/count_tokens` no longer depends on legacy state
- all Phase 8 and Phase 9 tests pass

For one release window, allow JSON config only to print a migration error:

```text
JSON oc-go-cc config is no longer supported by serve.
Run `llm-proxy init` to create TOML config, then copy model/API settings.
```

`serve --config old.json` must exit non-zero before constructing `AppState`.
It must not start the server with old JSON config. If `$OC_GO_CC_CONFIG` is
present, print the same migration error unless an explicit TOML `--config` or
`$LLM_PROXY_CONFIG` is provided.

Do not silently translate old scenario JSON into new model routes. That would
preserve the wrong mental model.

After this phase, remove `LegacyState` from `AppState` unless a still-mounted
route has a documented compile-time dependency on it. The expected result is no
legacy state.

Also make the new runtime fields non-optional:

```rust
pub app_config: Arc<AppConfig>,
pub providers: Arc<ProviderRegistry>,
```

Remove `app_config()` and `providers()` option helpers if they only existed to
bridge Phase 7 JSON compatibility. After Phase 10, serving requires TOML config.

### Tests

If CLI tests are not present, add unit tests for pure helpers:

- config path resolution prefers CLI path
- config path resolution supports `$LLM_PROXY_CONFIG`
- provider directory path is next to config file
- generated TOML parses as `AppConfig`
- generated provider TOML parses as `ProviderFile`

### Gate

```sh
cargo test -p llm-proxy
cargo test --workspace
```
