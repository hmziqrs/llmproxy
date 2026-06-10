use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur in `llm-proxy-core`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Failed to read a config file from disk.
    #[error("failed to read config at {path}: {source}")]
    ConfigLoad {
        /// Path to the config file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to parse a TOML config file.
    #[error("failed to parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),
    /// Config validation failed.
    #[error("config validation error: {message}")]
    ConfigValidation {
        /// Human-readable description of the validation failure.
        message: String,
    },
    /// Provider registry construction or protocol validation failed.
    #[error("provider resolution error: {message}")]
    ProviderResolution {
        /// Human-readable description of the resolution failure.
        message: String,
    },
}
