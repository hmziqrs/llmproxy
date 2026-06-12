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
        /// The original validation error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Provider registry construction or protocol validation failed.
    #[error("provider resolution error: {message}")]
    ProviderResolution {
        /// Human-readable description of the resolution failure.
        message: String,
        /// The original error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl CoreError {
    /// Create a [`CoreError::ConfigValidation`] from a message alone.
    pub fn config_validation(message: impl Into<String>) -> Self {
        Self::ConfigValidation {
            message: message.into(),
            source: None,
        }
    }

    /// Create a [`CoreError::ConfigValidation`] from a message and a source error.
    pub fn config_validation_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::ConfigValidation {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Create a [`CoreError::ProviderResolution`] from a message alone.
    pub fn provider_resolution(message: impl Into<String>) -> Self {
        Self::ProviderResolution {
            message: message.into(),
            source: None,
        }
    }

    /// Create a [`CoreError::ProviderResolution`] from a message and a source error.
    pub fn provider_resolution_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::ProviderResolution {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}
