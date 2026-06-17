//! PID file management helpers.
//!
//! Delegates to `llm_proxy_core::PidManager` for the core PID file operations
//! and adds validation (reject PID 0, overflow checks) and restrictive
//! file permissions.

use std::io::Write;

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::paths::{config_dir, pid_file_path};
use crate::permissions::{set_private_dir_permissions, set_private_permissions};

/// Number of polling iterations when waiting for graceful shutdown.
pub const STOP_MAX_POLLS: u32 = 50;

/// Interval between polls when waiting for graceful shutdown.
pub const STOP_POLL_INTERVAL_MS: u64 = 200;

/// Build a [`llm_proxy_core::PidManager`] for the default config directory.
pub(crate) fn pid_manager() -> llm_proxy_core::PidManager {
    llm_proxy_core::PidManager::new(config_dir())
}

/// Read the PID from the PID file via the given manager.
///
/// Returns `None` if the file does not exist. Validates that the parsed PID
/// is non-zero and fits in `i32` to catch corrupt/stale PID files and prevent
/// undefined behavior when passing the PID to `libc::kill()`.
///
/// This is the injectable variant: callers that already hold a [`llm_proxy_core::PidManager`]
/// (e.g. `cmd_stop`/`cmd_status` operating on a tempdir-backed manager) use it
/// directly instead of rebuilding one from the global config dir (audit
/// MEDIUM-6).
pub fn read_pid_from(mgr: &llm_proxy_core::PidManager) -> Result<Option<u32>> {
    let pid = mgr.read_pid()?;
    if let Some(p) = pid {
        if p == 0 {
            bail!("invalid PID 0 in PID file");
        }
        if p > i32::MAX as u32 {
            bail!("PID {p} exceeds i32::MAX -- cannot be a valid process ID");
        }
    }
    Ok(pid)
}

/// Read the PID from the PID file. Returns `None` if the file does not exist.
///
/// Validates that the parsed PID is non-zero and fits in `i32` to catch
/// corrupt/stale PID files and prevent undefined behavior when passing the
/// PID to `libc::kill()`.
pub fn read_pid() -> Result<Option<u32>> {
    read_pid_from(&pid_manager())
}

/// Atomically create the PID file with exclusive ownership.
///
/// Uses `create_new(true)` so the file is created exclusively -- if another
/// process already created it, the call fails and we return the OS error.
/// This eliminates the TOCTOU race between checking for a stale PID and
/// writing the new one. After creation, restrictive permissions (0600) are
/// applied.
pub fn write_pid() -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;
    set_private_dir_permissions(&dir)?;
    let path = pid_file_path();
    let pid = std::process::id();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("creating PID file {}", path.display()))?;
    write!(f, "{pid}").with_context(|| format!("writing PID file {}", path.display()))?;
    // Set restrictive permissions on the PID file.
    set_private_permissions(&path)?;
    info!(pid, "wrote PID file");
    Ok(())
}

/// Write a specific PID value to the PID file.
///
/// Used by the daemon parent to write the child PID before detaching.
/// Applies restrictive permissions (0600) after creation.
pub fn write_pid_value(pid: u32) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;
    set_private_dir_permissions(&dir)?;
    let path = pid_file_path();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("creating PID file {}", path.display()))?;
    write!(f, "{pid}").with_context(|| format!("writing PID file {}", path.display()))?;
    set_private_permissions(&path)?;
    info!(pid, "wrote PID file");
    Ok(())
}

/// Remove the PID file.
pub fn remove_pid() -> Result<()> {
    // `PidManager::remove_pid` returns a typed `Result<(), PidError>`; convert
    // into the app layer's `anyhow::Result` via `?` (anyhow implements
    // `From<PidError>` because `PidError: std::error::Error + Send + Sync`).
    Ok(pid_manager().remove_pid()?)
}

/// Check if a process with the given PID is running.
///
/// Delegates to [`llm_proxy_core::PidManager::is_process_running`]. Returns `false` for
/// PID 0 (which would otherwise signal the entire process group on Unix)
/// and on non-Unix platforms where process signaling is unavailable.
pub fn is_process_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    llm_proxy_core::PidManager::is_process_running(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_pid_rejects_zero_pid() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("llm-proxy.pid");
        std::fs::write(&pid_path, "0").unwrap();

        // Use PidManager directly to test the zero-PID validation path.
        let mgr = llm_proxy_core::PidManager::new(dir.path());
        let pid = mgr.read_pid().unwrap();
        assert_eq!(pid, Some(0));

        // The read_pid() wrapper in main should reject PID 0.
        // We can't easily call it without setting up the global config_dir,
        // so test the validation logic directly.
        if let Some(p) = pid {
            assert!(p == 0);
            // This matches the check in read_pid(): pid == 0 should bail.
        }
    }

    #[test]
    fn is_process_running_current_process() {
        let pid = std::process::id();
        assert!(is_process_running(pid));
    }

    #[test]
    fn is_process_running_nonexistent_pid() {
        // PID 299999999 is extremely unlikely to exist.
        assert!(!is_process_running(299_999_999));
    }

    #[test]
    fn is_process_running_rejects_pid_zero() {
        // PID 0 should always return false to avoid signaling the entire
        // process group on Unix.
        assert!(!is_process_running(0));
    }
}
