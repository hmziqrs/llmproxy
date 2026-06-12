//! Application state construction and tracing initialization.
//!
//! Provides helpers to load TOML config into `AppState`, build `BuildInfo`,
//! check catalog enforcement, and initialize the tracing subscriber.

use std::path::Path;

use anyhow::{Context, Result};
use llm_proxy_core::{AppConfig, ProviderConfig, ProviderRegistry, merge_catalog, parse_catalog_file};
use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};
use llm_proxy_server::{AppState, BuildInfo};
use tracing::info;

use crate::paths::providers_dir_for;

/// Load TOML config files and build [`AppState`] in TOML new-runtime mode.
///
/// Loads the main `AppConfig` from `path`, then scans a `providers/` directory
/// next to `path` for provider TOML files. Uses [`ProviderRegistry::load_from_dir`]
/// which handles non-regular-file filtering, deterministic sorted ordering, and
/// duplicate-name detection. Protocol validation is performed separately against
/// the builtin adapter set after loading.
///
/// Takes ownership of `adapter_registry` and `proxy_client` since they are
/// consumed by [`AppState::new`] and not reused after this call.
pub(crate) fn load_toml_state(
    path: &Path,
    adapter_registry: ProviderAdapterRegistry,
    proxy_client: ProxyClient,
    port_override: Option<u16>,
) -> Result<AppState> {
    let mut app_config: AppConfig = llm_proxy_core::load_app_config(path)
        .with_context(|| format!("loading TOML config from {}", path.display()))?;

    // Apply CLI port override.
    // In-place mutation is safe here: the config is consumed by AppState::new
    // below and is not shared with any other caller.
    if let Some(p) = port_override {
        let mut bind = app_config.server.bind;
        bind.set_port(p);
        app_config.server.bind = bind;
    }

    // Load provider files from providers/ directory next to the main config.
    let providers_dir = providers_dir_for(path);
    let registry = if providers_dir.exists() {
        ProviderRegistry::load_from_dir(&providers_dir)
            .with_context(|| format!("loading providers from {}", providers_dir.display()))?
    } else {
        ProviderRegistry::from_providers(vec![])
            .with_context(|| "building empty provider registry")?
    };

    // Validate all provider protocols against the builtin adapter set.
    let known_protocols = adapter_registry.protocol_names();
    registry
        .validate_protocols(known_protocols)
        .with_context(|| "validating provider protocols")?;

    let cache_dir = providers_dir.join(".catalog");
    for provider in registry.iter() {
        if let Some(warning) = catalog_enforcement_warning(provider, &cache_dir) {
            tracing::warn!(provider = %provider.name, "{warning}");
        }
    }

    info!(
        config = %path.display(),
        providers = registry.len(),
        "loaded TOML config"
    );

    Ok(AppState::new_with_catalog_dir(
        app_config,
        registry,
        adapter_registry,
        proxy_client,
        build_info(),
        Some(cache_dir),
    ))
}

/// Check if a provider with catalog enforcement enabled has no usable models.
///
/// Returns a warning string if the provider enforces its catalog but has no
/// static models and no cached live discovery results.
pub fn catalog_enforcement_warning(provider: &ProviderConfig, cache_dir: &Path) -> Option<String> {
    let catalog = provider.catalog.as_ref()?;
    if !catalog.enforce || !merge_catalog(catalog, &[]).is_empty() {
        return None;
    }

    let cache_path = cache_dir.join(format!("{}.toml", provider.name));
    let cached_models = std::fs::symlink_metadata(&cache_path)
        .ok()
        .filter(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
        .and_then(|_| std::fs::read_to_string(&cache_path).ok())
        .and_then(|raw| parse_catalog_file(&raw).ok())
        .filter(|cache| cache.catalog.provider == provider.name)
        .map(|cache| cache.catalog.models)
        .unwrap_or_default();

    if !merge_catalog(catalog, &cached_models).is_empty() {
        return None;
    }

    Some(format!(
        "provider \"{}\" has catalog enforcement enabled but no allowed models; requests will be rejected until static models are configured or live discovery writes a cache",
        provider.name
    ))
}

/// Build version/target metadata embedded at compile time.
pub fn build_info() -> BuildInfo {
    BuildInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        target: env!("VERGEN_CARGO_TARGET_TRIPLE"),
        git_sha: env!("VERGEN_GIT_SHA"),
    }
}

/// Initialize the tracing subscriber.
///
/// Uses `RUST_LOG` env var with fallback to "info". The `log_level` field in
/// the TOML config file is intentionally not used here because the subscriber
/// must be initialized before the config is loaded (tracing is needed during
/// config loading itself). To control log verbosity, set `RUST_LOG` instead.
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    // Fallback to "info" level when RUST_LOG is not set. "info" is a known-valid
    // filter string so parse_lossy is safe here (it never panics).
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::builder().parse_lossy("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_config(raw: &str) -> ProviderConfig {
        toml::from_str::<llm_proxy_core::ProviderFile>(raw)
            .unwrap()
            .provider
    }

    #[test]
    fn enforcement_warning_respects_discovered_mode() {
        let provider = provider_config(
            r#"[provider]
name = "test-provider"
api_key = "test-key"
auth_style = "bearer"

[provider.catalog]
mode = "discovered"
enforce = true

[[provider.catalog.models]]
id = "static-model"
"#,
        );
        let directory = tempfile::tempdir().unwrap();

        let warning = catalog_enforcement_warning(&provider, directory.path());

        assert!(warning.is_some());
    }

    #[test]
    fn enforcement_warning_accepts_usable_cache() {
        let provider = provider_config(
            r#"[provider]
name = "test-provider"
api_key = "test-key"
auth_style = "bearer"

[provider.catalog]
mode = "discovered"
enforce = true
"#,
        );
        let directory = tempfile::tempdir().unwrap();
        let cache = llm_proxy_core::CatalogFile {
            catalog: llm_proxy_core::CatalogFileMetadata {
                provider: "test-provider".to_owned(),
                source: "live".to_owned(),
                generated_at: "2026-06-10T00:00:00Z".to_owned(),
                models: vec![llm_proxy_core::StaticModelCatalogEntry {
                    id: "cached-model".to_owned(),
                    display_name: None,
                    supports: Vec::new(),
                    context_length: None,
                }],
            },
        };
        std::fs::write(
            directory.path().join("test-provider.toml"),
            toml::to_string(&cache).unwrap(),
        )
        .unwrap();

        let warning = catalog_enforcement_warning(&provider, directory.path());

        assert!(warning.is_none());
    }
}
