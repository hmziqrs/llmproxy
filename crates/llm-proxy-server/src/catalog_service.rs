//! Runtime provider model catalog cache and refresh coordination.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use llm_proxy_core::{
    CatalogFile, CatalogFileMetadata, ProviderConfig, StaticModelCatalogEntry, merge_catalog,
};
use llm_proxy_provider::{DiscoveryClient, ProviderError};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// Mutable catalog state kept separate from immutable provider routing config.
#[derive(Debug)]
pub struct ModelCatalogService {
    cache_dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, CatalogFile>>,
    refresh_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    last_refresh: Mutex<HashMap<String, Instant>>,
    discovery: DiscoveryClient,
}

impl ModelCatalogService {
    /// Create a catalog service. When `cache_dir` is absent, disk caching is disabled.
    #[must_use]
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            cache: RwLock::new(HashMap::new()),
            refresh_locks: Mutex::new(HashMap::new()),
            last_refresh: Mutex::new(HashMap::new()),
            discovery: DiscoveryClient::default(),
        }
    }

    /// Return the merged, filtered catalog, optionally refreshing it live.
    ///
    /// # Errors
    ///
    /// Returns an error when an explicitly requested live refresh fails or a
    /// cache file is unsafe or malformed.
    pub async fn catalog(
        &self,
        provider: &ProviderConfig,
        refresh_live: bool,
    ) -> Result<Vec<StaticModelCatalogEntry>, ProviderError> {
        self.load_disk_cache(provider).await?;
        let config = provider.catalog.clone().unwrap_or_default();
        let discovery_enabled = !matches!(config.mode, llm_proxy_core::ProviderCatalogMode::Static)
            && provider.discovery.is_some();
        if refresh_live && discovery_enabled && !self.cache_is_fresh(provider).await {
            if let Err(error) = self.refresh(provider).await {
                let has_fallback = self.cache.read().await.contains_key(&provider.name)
                    || provider
                        .catalog
                        .as_ref()
                        .is_some_and(|catalog| !catalog.models.is_empty());
                if !has_fallback {
                    return Err(error);
                }
                tracing::warn!(provider = %provider.name, error = %error, "catalog refresh failed; using stale/static catalog");
            }
        }

        let discovered = self
            .cache
            .read()
            .await
            .get(&provider.name)
            .map(|file| file.catalog.models.clone())
            .unwrap_or_default();
        Ok(merge_catalog(&config, &discovered))
    }

    async fn refresh(&self, provider: &ProviderConfig) -> Result<(), ProviderError> {
        let requested_at = Instant::now();
        let lock = {
            let mut locks = self.refresh_locks.lock().map_err(|_| {
                ProviderError::InvalidConfig("catalog refresh lock is poisoned".to_owned())
            })?;
            locks
                .entry(provider.name.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        if self
            .last_refresh
            .lock()
            .map_err(|_| {
                ProviderError::InvalidConfig("catalog refresh state is poisoned".to_owned())
            })?
            .get(&provider.name)
            .is_some_and(|completed_at| *completed_at >= requested_at)
        {
            return Ok(());
        }

        let models = self.discovery.discover(provider).await?;
        let file = CatalogFile {
            catalog: CatalogFileMetadata {
                provider: provider.name.clone(),
                source: "live".to_owned(),
                generated_at: now_rfc3339(),
                models,
            },
        };
        if let Some(cache_dir) = &self.cache_dir {
            write_catalog_atomic(cache_dir, &file)?;
        }
        self.cache.write().await.insert(provider.name.clone(), file);
        self.last_refresh
            .lock()
            .map_err(|_| {
                ProviderError::InvalidConfig("catalog refresh state is poisoned".to_owned())
            })?
            .insert(provider.name.clone(), Instant::now());
        Ok(())
    }

    async fn cache_is_fresh(&self, provider: &ProviderConfig) -> bool {
        let ttl = provider.catalog.clone().unwrap_or_default().cache_ttl;
        self.cache
            .read()
            .await
            .get(&provider.name)
            .is_some_and(|file| {
                time::OffsetDateTime::parse(
                    &file.catalog.generated_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .ok()
                .map(|generated_at| time::OffsetDateTime::now_utc() - generated_at)
                .is_some_and(|age| {
                    age.whole_seconds() >= 0 && age.whole_seconds() < ttl.as_secs() as i64
                })
            })
    }

    async fn load_disk_cache(&self, provider: &ProviderConfig) -> Result<(), ProviderError> {
        if self.cache.read().await.contains_key(&provider.name) {
            return Ok(());
        }
        let Some(cache_dir) = &self.cache_dir else {
            return Ok(());
        };
        let path = cache_dir.join(format!("{}.toml", provider.name));
        if !path.exists() {
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(invalid_cache)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ProviderError::InvalidConfig(format!(
                "catalog cache must be a regular non-symlink file: {}",
                path.display()
            )));
        }
        let raw = std::fs::read_to_string(&path).map_err(invalid_cache)?;
        let file: CatalogFile = toml::from_str(&raw).map_err(|error| {
            ProviderError::InvalidConfig(format!(
                "failed to parse catalog cache {}: {error}",
                path.display()
            ))
        })?;
        if file.catalog.provider != provider.name {
            return Err(ProviderError::InvalidConfig(format!(
                "catalog cache provider mismatch in {}",
                path.display()
            )));
        }
        self.cache.write().await.insert(provider.name.clone(), file);
        Ok(())
    }
}

fn invalid_cache(error: std::io::Error) -> ProviderError {
    ProviderError::InvalidConfig(format!("catalog cache I/O failed: {error}"))
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Persist a catalog using a temporary file and atomic rename.
///
/// # Errors
///
/// Returns an error for unsafe paths, serialization failures, or filesystem errors.
pub fn write_catalog_atomic(cache_dir: &Path, file: &CatalogFile) -> Result<(), ProviderError> {
    std::fs::create_dir_all(cache_dir).map_err(invalid_cache)?;
    let metadata = std::fs::symlink_metadata(cache_dir).map_err(invalid_cache)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProviderError::InvalidConfig(format!(
            "catalog cache directory must be a non-symlink directory: {}",
            cache_dir.display()
        )));
    }
    let destination = cache_dir.join(format!("{}.toml", file.catalog.provider));
    if destination.exists() {
        let metadata = std::fs::symlink_metadata(&destination).map_err(invalid_cache)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ProviderError::InvalidConfig(format!(
                "catalog destination is not a regular file: {}",
                destination.display()
            )));
        }
    }
    let temporary = cache_dir.join(format!(".{}.toml.tmp", file.catalog.provider));
    let body = toml::to_string_pretty(file).map_err(|error| {
        ProviderError::InvalidConfig(format!("failed to serialize catalog cache: {error}"))
    })?;
    std::fs::write(&temporary, body).map_err(invalid_cache)?;
    std::fs::rename(&temporary, &destination).map_err(invalid_cache)?;
    Ok(())
}

impl Default for ModelCatalogService {
    fn default() -> Self {
        Self::new(None)
    }
}
