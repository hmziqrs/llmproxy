use std::sync::Arc;

use llm_proxy_core::Config;

/// Application state shared with every handler.
///
/// Cheap to clone: `Config` is small and `Arc`-wrapped fields share
/// allocation. Required by axum's `State` extractor. `Debug` is
/// mandatory under the `missing_debug_implementations` lint.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Server configuration.
    pub config: Arc<Config>,
    /// Build identifier reported by `/version`.
    pub build: Arc<BuildInfo>,
}

/// Static information about this binary.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// Package name.
    pub name: &'static str,
    /// Package version.
    pub version: &'static str,
    /// Target triple the binary was compiled for.
    pub target: &'static str,
    /// Git commit SHA the binary was built from.
    pub git_sha: &'static str,
}

impl AppState {
    /// Construct a new `AppState` from config and build info.
    #[must_use]
    pub fn new(config: Config, build: BuildInfo) -> Self {
        Self {
            config: Arc::new(config),
            build: Arc::new(build),
        }
    }
}
