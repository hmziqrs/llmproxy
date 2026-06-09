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

#[derive(Debug, Clone)]
struct RefreshOutcome {
    completed_at: Instant,
    error: Option<String>,
}

/// Mutable catalog state kept separate from immutable provider routing config.
#[derive(Debug)]
pub struct ModelCatalogService {
    cache_dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, CatalogFile>>,
    refresh_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    refresh_outcomes: Mutex<HashMap<String, RefreshOutcome>>,
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
            refresh_outcomes: Mutex::new(HashMap::new()),
            discovery: DiscoveryClient::default(),
        }
    }

    /// Return the merged, filtered catalog, optionally refreshing it live.
    ///
    /// # Errors
    ///
    /// Returns an error when a live refresh fails without stale/static fallback,
    /// or when a cache file is unsafe or malformed.
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
        if let Some(outcome) = self
            .refresh_outcomes
            .lock()
            .map_err(|_| {
                ProviderError::InvalidConfig("catalog refresh state is poisoned".to_owned())
            })?
            .get(&provider.name)
            .filter(|outcome| outcome.completed_at >= requested_at)
            .cloned()
        {
            return outcome
                .error
                .map_or(Ok(()), |error| Err(ProviderError::InvalidConfig(error)));
        }

        let models = match self.discovery.discover(provider).await {
            Ok(models) => models,
            Err(error) => {
                self.record_refresh_outcome(&provider.name, Some(error.to_string()))?;
                return Err(error);
            }
        };
        let file = CatalogFile {
            catalog: CatalogFileMetadata {
                provider: provider.name.clone(),
                source: "live".to_owned(),
                generated_at: now_rfc3339(),
                models,
            },
        };
        if let Some(cache_dir) = &self.cache_dir {
            if let Err(error) = write_catalog_atomic(cache_dir, &file) {
                self.record_refresh_outcome(&provider.name, Some(error.to_string()))?;
                return Err(error);
            }
        }
        self.cache.write().await.insert(provider.name.clone(), file);
        self.record_refresh_outcome(&provider.name, None)?;
        Ok(())
    }

    fn record_refresh_outcome(
        &self,
        provider: &str,
        error: Option<String>,
    ) -> Result<(), ProviderError> {
        self.refresh_outcomes
            .lock()
            .map_err(|_| {
                ProviderError::InvalidConfig("catalog refresh state is poisoned".to_owned())
            })?
            .insert(
                provider.to_owned(),
                RefreshOutcome {
                    completed_at: Instant::now(),
                    error,
                },
            );
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
    if !is_safe_provider_slug(&file.catalog.provider) {
        return Err(ProviderError::InvalidConfig(
            "catalog provider must be a lowercase URL-safe slug".to_owned(),
        ));
    }
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
    let temporary = cache_dir.join(format!(
        ".{}.{}.toml.tmp",
        file.catalog.provider,
        uuid::Uuid::new_v4()
    ));
    let body = toml::to_string_pretty(file).map_err(|error| {
        ProviderError::InvalidConfig(format!("failed to serialize catalog cache: {error}"))
    })?;
    let write_result = (|| {
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(invalid_cache)?;
        output.write_all(body.as_bytes()).map_err(invalid_cache)?;
        output.sync_all().map_err(invalid_cache)?;
        std::fs::rename(&temporary, &destination).map_err(invalid_cache)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result?;
    Ok(())
}

fn is_safe_provider_slug(provider: &str) -> bool {
    !provider.is_empty()
        && provider.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

impl Default for ModelCatalogService {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::{Router, extract::State, http::StatusCode, routing::get};
    use llm_proxy_core::{
        AuthStyle, ProviderCatalogConfig, ProviderCatalogMode, ProviderDiscoveryConfig,
        ProviderDiscoveryKind, ProviderRoutesConfig,
    };

    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("llm-proxy-catalog-test-{}", uuid::Uuid::new_v4()))
    }

    fn provider(endpoint: String) -> ProviderConfig {
        ProviderConfig {
            name: "test-provider".to_owned(),
            api_key: "test-key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: Some(ProviderDiscoveryConfig {
                kind: ProviderDiscoveryKind::OpenAiCompatibleModels,
                endpoint,
                headers: HashMap::new(),
            }),
            catalog: Some(ProviderCatalogConfig {
                mode: ProviderCatalogMode::Discovered,
                enforce: false,
                cache_ttl: Duration::from_secs(60),
                allow: vec!["*".to_owned()],
                deny: Vec::new(),
                models: Vec::new(),
            }),
        }
    }

    fn catalog_file(generated_at: &str) -> CatalogFile {
        CatalogFile {
            catalog: CatalogFileMetadata {
                provider: "test-provider".to_owned(),
                source: "live".to_owned(),
                generated_at: generated_at.to_owned(),
                models: vec![StaticModelCatalogEntry {
                    id: "cached-model".to_owned(),
                    display_name: None,
                    supports: Vec::new(),
                    context_length: None,
                }],
            },
        }
    }

    async fn failing_models(State(count): State<Arc<AtomicUsize>>) -> StatusCode {
        count.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(75)).await;
        StatusCode::INTERNAL_SERVER_ERROR
    }

    async fn failing_server() -> (String, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/models", get(failing_models))
            .with_state(count.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/models"), count)
    }

    #[test]
    fn atomic_catalog_write_round_trips() {
        let directory = temp_dir();
        let file = catalog_file("2026-06-10T00:00:00Z");
        write_catalog_atomic(&directory, &file).unwrap();
        let raw = std::fs::read_to_string(directory.join("test-provider.toml")).unwrap();
        let decoded: CatalogFile = toml::from_str(&raw).unwrap();
        assert_eq!(decoded.catalog.models[0].id, "cached-model");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_catalog_write_rejects_unsafe_provider_name() {
        let directory = temp_dir();
        let mut file = catalog_file("2026-06-10T00:00:00Z");
        file.catalog.provider = "../outside".to_owned();
        assert!(write_catalog_atomic(&directory, &file).is_err());
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn concurrent_failed_refreshes_are_single_flight() {
        let (endpoint, count) = failing_server().await;
        let provider = Arc::new(provider(endpoint));
        let service = Arc::new(ModelCatalogService::new(None));
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let provider = provider.clone();
            let service = service.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                service.catalog(&provider, true).await
            }));
        }
        for task in tasks {
            assert!(task.await.unwrap().is_err());
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_refresh_uses_stale_disk_catalog() {
        let (endpoint, count) = failing_server().await;
        let directory = temp_dir();
        write_catalog_atomic(&directory, &catalog_file("2020-01-01T00:00:00Z")).unwrap();
        let service = ModelCatalogService::new(Some(directory.clone()));
        let models = service.catalog(&provider(endpoint), true).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "cached-model");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
