//! Provider model catalog types, merging, and filtering.

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{CoreError, ProviderCatalogConfig, ProviderCatalogMode, StaticModelCatalogEntry};

/// Metadata stored alongside a persisted provider catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFileMetadata {
    /// Provider that owns the catalog.
    pub provider: String,
    /// Catalog source, normally `live`.
    pub source: String,
    /// RFC 3339 timestamp when the catalog was generated.
    pub generated_at: String,
    /// Discovered model entries.
    #[serde(default)]
    pub models: Vec<StaticModelCatalogEntry>,
}

/// On-disk catalog cache file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFile {
    /// Cache metadata.
    pub catalog: CatalogFileMetadata,
}

/// Parse a persisted catalog cache file.
pub fn parse_catalog_file(raw: &str) -> Result<CatalogFile, CoreError> {
    toml::from_str(raw).map_err(CoreError::ConfigParse)
}

/// Merge static and discovered catalog entries according to provider config.
#[must_use]
pub fn merge_catalog(
    config: &ProviderCatalogConfig,
    discovered: &[StaticModelCatalogEntry],
) -> Vec<StaticModelCatalogEntry> {
    let mut entries = BTreeMap::<String, StaticModelCatalogEntry>::new();

    if matches!(
        config.mode,
        ProviderCatalogMode::Discovered | ProviderCatalogMode::Hybrid
    ) {
        for model in discovered {
            entries.insert(model.id.clone(), model.clone());
        }
    }
    if matches!(
        config.mode,
        ProviderCatalogMode::Static | ProviderCatalogMode::Hybrid
    ) {
        for model in &config.models {
            entries.insert(model.id.clone(), model.clone());
        }
    }

    entries
        .into_values()
        .filter(|model| model_allowed(&model.id, &config.allow, &config.deny))
        .collect()
}

/// Return whether a model ID passes the configured glob filters.
#[must_use]
pub fn model_allowed(model: &str, allow: &[String], deny: &[String]) -> bool {
    let allowed = allow.is_empty() || allow.iter().any(|pattern| glob_matches(pattern, model));
    let denied = deny.iter().any(|pattern| glob_matches(pattern, model));
    let explicitly_allowed = allow
        .iter()
        .any(|pattern| pattern != "*" && glob_matches(pattern, model));
    allowed && (!denied || explicitly_allowed)
}

/// Cache of compiled glob-to-regex patterns.
///
/// Avoids recompiling the same glob pattern on every call to [`glob_matches`].
/// Uses `LazyLock` for thread-safe one-time initialization per unique pattern.
static GLOB_CACHE: std::sync::Mutex<Vec<(String, Regex)>> = std::sync::Mutex::new(Vec::new());

fn glob_matches(pattern: &str, value: &str) -> bool {
    let mut cache = GLOB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // Check if we already compiled this pattern.
    if let Some((_, regex)) = cache.iter().find(|(p, _)| p == pattern) {
        return regex.is_match(value);
    }
    // Compile and cache the new pattern.
    let mut expression = String::with_capacity(pattern.len() + 2);
    expression.push('^');
    for ch in pattern.chars() {
        match ch {
            '*' => expression.push_str(".*"),
            '?' => expression.push('.'),
            other => expression.push_str(&regex::escape(&other.to_string())),
        }
    }
    expression.push('$');
    let regex = match Regex::new(&expression) {
        Ok(re) => re,
        Err(_) => return false,
    };
    let matched = regex.is_match(value);
    cache.push((pattern.to_owned(), regex));
    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderRouteKind;
    use std::time::Duration;

    fn model(id: &str, display_name: Option<&str>) -> StaticModelCatalogEntry {
        StaticModelCatalogEntry {
            id: id.to_owned(),
            display_name: display_name.map(str::to_owned),
            supports: vec![ProviderRouteKind::ChatCompletions],
            context_length: None,
        }
    }

    fn config(mode: ProviderCatalogMode) -> ProviderCatalogConfig {
        ProviderCatalogConfig {
            mode,
            enforce: false,
            cache_ttl: Duration::from_secs(60),
            allow: vec!["*".to_owned()],
            deny: Vec::new(),
            models: vec![model("shared", Some("static"))],
        }
    }

    #[test]
    fn hybrid_static_metadata_wins() {
        let merged = merge_catalog(
            &config(ProviderCatalogMode::Hybrid),
            &[model("shared", Some("discovered")), model("other", None)],
        );
        assert_eq!(merged[1].display_name.as_deref(), Some("static"));
    }

    #[test]
    fn deny_filter_excludes_matching_models() {
        assert!(!model_allowed(
            "model-preview",
            &["*".to_owned()],
            &["*-preview".to_owned()]
        ));
    }

    #[test]
    fn explicit_allow_overrides_deny() {
        assert!(model_allowed(
            "model-preview",
            &["*".to_owned(), "model-preview".to_owned()],
            &["*-preview".to_owned()]
        ));
    }

    #[test]
    fn parse_catalog_file_rejects_invalid_toml() {
        let result = parse_catalog_file("this is not valid toml [[");
        assert!(
            result.is_err(),
            "expected error for invalid TOML input"
        );
        assert!(
            matches!(result.unwrap_err(), CoreError::ConfigParse(_)),
            "expected CoreError::ConfigParse variant"
        );
    }

    #[test]
    fn discovered_mode_only_uses_discovered_entries() {
        let cfg = config(ProviderCatalogMode::Discovered);
        let merged = merge_catalog(
            &cfg,
            &[model("discovered-a", None), model("discovered-b", None)],
        );
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|m| m.id == "discovered-a"));
        assert!(merged.iter().any(|m| m.id == "discovered-b"));
        // Static "shared" model should NOT be present in Discovered mode.
        assert!(!merged.iter().any(|m| m.id == "shared"));
    }

    #[test]
    fn static_mode_only_uses_static_entries() {
        let cfg = config(ProviderCatalogMode::Static);
        let merged = merge_catalog(
            &cfg,
            &[model("discovered-a", None), model("shared", Some("discovered"))],
        );
        // Only static entries should be present.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "shared");
        // The static display_name should win since only static entries are used.
        assert_eq!(merged[0].display_name.as_deref(), Some("static"));
    }

    #[test]
    fn catalog_file_uses_nested_catalog_model_tables() {
        let file = CatalogFile {
            catalog: CatalogFileMetadata {
                provider: "example".to_owned(),
                source: "live".to_owned(),
                generated_at: "2026-06-09T00:00:00Z".to_owned(),
                models: vec![model("example-model", None)],
            },
        };
        let encoded = toml::to_string(&file).expect("catalog TOML");
        assert!(encoded.contains("[[catalog.models]]"));
        let decoded: CatalogFile = toml::from_str(&encoded).expect("catalog round trip");
        assert_eq!(decoded.catalog.models[0].id, "example-model");
    }
}
