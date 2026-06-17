//! Path resolution helpers.
//!
//! Provides functions for resolving config directories, file paths,
//! provider directories, and platform-specific autostart paths.
//! Includes a minimal `home_env` module for home directory resolution.

use std::path::{Path, PathBuf};

/// Config directory: `~/.config/llm-proxy/`.
///
/// Falls back to `./.config/llm-proxy/` (relative to current working directory)
/// when `$HOME` and `$USERPROFILE` are both unset. This is unlikely in practice
/// but avoids a panic. A warning is emitted to stderr in that case since the
/// resolved path depends on the current working directory.
pub fn config_dir() -> PathBuf {
    let home = home_env::home_dir();
    if home.is_none() {
        eprintln!(
            "warning: HOME and USERPROFILE are both unset; using relative \
             config path (depends on current working directory)"
        );
    }
    home.unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("llm-proxy")
}

/// Injectable path bundle used by the `stop`/`status` commands (audit
/// MEDIUM-6).
///
/// Carries the [`llm_proxy_core::PidManager`] that resolves the PID file plus the config file
/// path that `status` reads for the `listen` address. Bundling them lets the
/// full command branching — stale-file cleanup, "no PID file found", and the
/// complete SIGTERM→poll→SIGKILL escalation — be exercised against a
/// tempdir-backed manager in tests instead of the process-global `config_dir`.
#[derive(Debug, Clone)]
pub struct CommandPaths {
    pid_manager: llm_proxy_core::PidManager,
    config_path: PathBuf,
}

impl CommandPaths {
    /// Build a [`CommandPaths`] from an explicit PID manager and config path.
    #[must_use]
    pub fn new(pid_manager: llm_proxy_core::PidManager, config_path: PathBuf) -> Self {
        Self {
            pid_manager,
            config_path,
        }
    }

    /// The PID file manager backing `stop`/`status`.
    #[must_use]
    pub fn pid_manager(&self) -> &llm_proxy_core::PidManager {
        &self.pid_manager
    }

    /// The config file `status` reads for the `listen` address.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

/// Build [`CommandPaths`] for the default config directory + default config
/// path (the values used by the real CLI).
#[must_use]
pub fn default_command_paths() -> CommandPaths {
    CommandPaths::new(
        llm_proxy_core::PidManager::new(config_dir()),
        default_config_path(),
    )
}

/// Config file path: `~/.config/llm-proxy/config.toml`.
pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// PID file path: `~/.config/llm-proxy/llm-proxy.pid`.
pub fn pid_file_path() -> PathBuf {
    config_dir().join("llm-proxy.pid")
}

/// Resolve the config file path.
///
/// Priority:
/// 1. Explicit CLI `--config` argument
/// 2. `$LLM_PROXY_CONFIG` env var
/// 3. Default path `~/.config/llm-proxy/config.toml`
pub fn resolve_config(cli_path: Option<&Path>) -> PathBuf {
    // 1. Explicit CLI path always wins.
    if let Some(p) = cli_path {
        return p.to_path_buf();
    }
    // 2. Preferred env var.
    if let Ok(p) = std::env::var("LLM_PROXY_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    default_config_path()
}

/// Returns the providers directory path, which is always `providers/`
/// next to the config file.
///
/// Falls back to `./providers` if the config path has no parent directory
/// (e.g. bare filename `config.toml` with no directory component).
pub fn providers_dir_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("providers")
}

/// Returns the launchd plist path on macOS.
///
/// Falls back to `./com.llm-proxy.plist` if home directory cannot be determined.
#[cfg(target_os = "macos")]
pub fn launchd_plist_path() -> PathBuf {
    home_env::home_dir()
        .map(|h| {
            h.join("Library")
                .join("LaunchAgents")
                .join("com.llm-proxy.plist")
        })
        .unwrap_or_else(|| PathBuf::from("com.llm-proxy.plist"))
}

/// Returns the XDG autostart directory on Linux.
///
/// Falls back to `./.config/autostart` if home directory cannot be determined.
#[cfg(target_os = "linux")]
pub fn linux_autostart_dir() -> PathBuf {
    home_env::home_dir()
        .map(|h| h.join(".config").join("autostart"))
        .unwrap_or_else(|| PathBuf::from(".config/autostart"))
}

// ---------------------------------------------------------------------------
// home_env helper (minimal, no external dep)
// ---------------------------------------------------------------------------

/// Minimal home-directory resolution without depending on the `dirs` crate.
///
/// Checks `$HOME` (macOS/Linux) and `$USERPROFILE` (Windows) only.
/// Does not fall back to `getpwuid()` or XDG defaults. This is intentional:
/// the `dirs` crate is avoided to minimize dependency tree, and these two
/// env vars cover all standard configurations.
pub(crate) mod home_env {
    use std::path::PathBuf;

    /// Returns the user's home directory.
    pub(crate) fn home_dir() -> Option<PathBuf> {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_config_uses_explicit_path() {
        let path = Path::new("/tmp/my-config.toml");
        let result = resolve_config(Some(path));
        assert_eq!(result, PathBuf::from("/tmp/my-config.toml"));
    }

    #[test]
    fn resolve_config_uses_default_when_no_args() {
        // Without LLM_PROXY_CONFIG set, should return default path.
        // We can't unset env vars in this test, so just verify the function
        // returns a path ending in config.toml.
        let result = resolve_config(None);
        assert!(result.to_string_lossy().ends_with("config.toml"));
    }
}
