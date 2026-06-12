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

impl From<crate::provider_config::ConfigValidationError> for CoreError {
    fn from(e: crate::provider_config::ConfigValidationError) -> Self {
        Self::ConfigValidation {
            message: e.to_string(),
            source: Some(Box::new(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn config_load_display_includes_path() {
        let err = CoreError::ConfigLoad {
            path: PathBuf::from("/tmp/test.toml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        let msg = err.to_string();
        assert!(msg.contains("/tmp/test.toml"), "display should contain path");
        assert!(msg.contains("failed to read config"));
    }

    #[test]
    fn config_load_has_source() {
        let err = CoreError::ConfigLoad {
            path: PathBuf::from("/tmp/test.toml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        assert!(err.source().is_some(), "ConfigLoad should have a source");
    }

    #[test]
    fn config_parse_display() {
        let err = CoreError::ConfigParse(toml::from_str::<toml::Value>("bad [[[").unwrap_err());
        let msg = err.to_string();
        assert!(msg.contains("failed to parse config"));
    }

    #[test]
    fn config_validation_from_message() {
        let err = CoreError::config_validation("test message");
        let msg = err.to_string();
        assert!(msg.contains("test message"));
        assert!(err.source().is_none());
    }

    #[test]
    fn config_validation_from_config_validation_error() {
        let validation_err = crate::provider_config::ConfigValidationError::EmptyProviderName;
        let core_err: CoreError = validation_err.into();
        assert!(core_err.source().is_some());
        let msg = core_err.to_string();
        assert!(msg.contains("provider name is empty"));
    }

    #[test]
    fn provider_resolution_from_message() {
        let err = CoreError::provider_resolution("something went wrong");
        let msg = err.to_string();
        assert!(msg.contains("something went wrong"));
        assert!(err.source().is_none());
    }
}
