use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Top-level server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind: SocketAddr,
    /// Maximum request duration before timeout. Accepts humantime
    /// strings in TOML, e.g. `request_timeout = "60s"`.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Public server name reported by `/version`.
    pub server_name: String,
}

impl Config {
    /// Load a config from a TOML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, crate::error::CoreError> {
        let path = path.as_ref();
        let raw =
            std::fs::read_to_string(path).map_err(|e| crate::error::CoreError::ConfigLoad {
                path: path.to_path_buf(),
                source: e,
            })?;
        toml::from_str(&raw).map_err(crate::error::CoreError::ConfigParse)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080"
                .parse()
                .expect("default socket addr is valid"),
            request_timeout: Duration::from_secs(60),
            server_name: env!("CARGO_PKG_NAME").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bind_is_localhost_or_all() {
        let cfg = Config::default();
        assert_eq!(cfg.bind.port(), 8080);
    }

    #[test]
    fn default_timeout_is_60_seconds() {
        let cfg = Config::default();
        assert_eq!(cfg.request_timeout, Duration::from_secs(60));
    }
}
