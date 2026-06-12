//! PID file management for daemon mode.
//!
//! Provides [`PidManager`] which handles creating, reading, and removing PID
//! files used to track a running daemon process. The config directory is
//! configurable so that tests can point at a temporary directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Manages a PID file for daemon process tracking.
#[derive(Debug, Clone)]
pub struct PidManager {
    /// Directory for config and PID files.
    config_dir: PathBuf,
    /// Path to the PID file.
    pid_file: PathBuf,
}

impl PidManager {
    /// Create a new PID manager for the given config directory.
    ///
    /// The PID file will be placed at `<config_dir>/llm-proxy.pid`.
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        let config_dir = config_dir.into();
        let pid_file = config_dir.join("llm-proxy.pid");
        Self {
            config_dir,
            pid_file,
        }
    }

    /// Return the config directory path.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Return the PID file path.
    pub fn pid_file(&self) -> &Path {
        &self.pid_file
    }

    /// Read the PID from the PID file. Returns `None` if the file does not exist.
    pub fn read_pid(&self) -> Result<Option<u32>> {
        let content = match std::fs::read_to_string(&self.pid_file) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("reading PID file {}", self.pid_file.display()));
            }
        };
        let pid: u32 = content
            .trim()
            .parse()
            .with_context(|| format!("parsing PID from {}", self.pid_file.display()))?;
        Ok(Some(pid))
    }

    /// Write the current process PID to the PID file.
    ///
    /// Creates the config directory if it does not already exist.
    /// The PID file is written atomically via a temporary file and rename,
    /// so readers never see a partial/empty file.
    pub fn write_pid(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating directory {}", self.config_dir.display()))?;
        let pid = std::process::id();

        // Write to a temporary file first, then rename atomically.
        let tmp_path = self.pid_file.with_extension("pid.tmp");
        {
            #[cfg(unix)]
            let mut f = {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o644)
                    .open(&tmp_path)
                    .with_context(|| format!("creating temp PID file {}", tmp_path.display()))?
            };
            #[cfg(not(unix))]
            let mut f = std::fs::File::create(&tmp_path)
                .with_context(|| format!("creating temp PID file {}", tmp_path.display()))?;

            write!(f, "{pid}")
                .with_context(|| format!("writing PID file {}", tmp_path.display()))?;
        }
        std::fs::rename(&tmp_path, &self.pid_file)
            .with_context(|| format!("renaming PID file {}", self.pid_file.display()))?;
        Ok(())
    }

    /// Remove the PID file.
    ///
    /// Silently succeeds if the file does not exist.
    pub fn remove_pid(&self) -> Result<()> {
        match std::fs::remove_file(&self.pid_file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e)
                .with_context(|| format!("removing PID file {}", self.pid_file.display())),
        }
    }

    /// Check if a process with the given PID is running.
    ///
    /// On Unix this uses `kill(pid, 0)` which only checks process existence
    /// without sending a signal. On non-Unix platforms this always returns
    /// `true` as a conservative fallback.
    ///
    /// # Notes
    ///
    /// - `EPERM` (permission denied) is treated as "running" because the
    ///   process exists even though we cannot signal it.
    /// - `ESRCH` (no such process) is treated as "not running".
    pub fn is_process_running(pid: u32) -> bool {
        #[cfg(unix)]
        {
            // SAFETY: kill(pid, 0) just checks if the process exists; it does
            // not send a signal on any Unix platform.
            let ret = unsafe { libc::kill(pid as i32, 0) };
            if ret == 0 {
                true
            } else {
                let err = std::io::Error::last_os_error();
                // EPERM means the process exists but we lack permission.
                err.raw_os_error() != Some(libc::ESRCH)
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::PidManager;

    /// TestWritePIDAndGetPID_RoundTrip: writing a PID and reading it back
    /// should return the same value.
    #[test]
    fn write_and_read_pid_roundtrip() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = PidManager::new(dir.path());

        // Write the PID of the current test process.
        mgr.write_pid().expect("write_pid");

        let pid = mgr.read_pid().expect("read_pid").expect("pid should exist");
        assert_eq!(pid, std::process::id());
    }

    /// TestGetPID_MissingFile: reading from a non-existent PID file should
    /// return `Ok(None)`.
    #[test]
    fn read_pid_missing_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = PidManager::new(dir.path());

        let result = mgr.read_pid().expect("read_pid should not error");
        assert!(result.is_none(), "expected None for missing PID file");
    }

    /// TestGetPID_InvalidContent: reading a PID file with non-numeric content
    /// should return an error.
    #[test]
    fn read_pid_invalid_content() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = PidManager::new(dir.path());

        // Write garbage to the PID file.
        std::fs::write(mgr.pid_file(), "not-a-number").expect("write garbage");

        let result = mgr.read_pid();
        assert!(result.is_err(), "expected error for invalid PID content");
    }

    /// TestIsProcessRunning_CurrentProcess: the current process PID should be
    /// reported as running.
    #[test]
    fn is_process_running_current_process() {
        let pid = std::process::id();
        assert!(
            PidManager::is_process_running(pid),
            "current process should be running"
        );
    }

    /// TestIsProcessRunning_NonexistentPID: a very large PID that almost
    /// certainly does not exist should be reported as not running.
    #[test]
    fn is_process_running_nonexistent_pid() {
        // PID 299999999 is extremely unlikely to exist on any system.
        let pid = 299_999_999;
        assert!(
            !PidManager::is_process_running(pid),
            "nonexistent PID should not be running"
        );
    }

    /// TestRemovePID: removing the PID file should delete it from disk.
    #[test]
    fn remove_pid_deletes_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = PidManager::new(dir.path());

        // Write then remove.
        mgr.write_pid().expect("write_pid");
        assert!(mgr.pid_file().exists(), "PID file should exist after write");
        mgr.remove_pid().expect("remove_pid");
        assert!(!mgr.pid_file().exists(), "PID file should be gone after remove");
    }

    /// TestRemovePID_MissingFile: removing a non-existent PID file should
    /// succeed silently.
    #[test]
    fn remove_pid_succeeds_when_missing() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = PidManager::new(dir.path());
        // No PID file was created. remove_pid should succeed.
        mgr.remove_pid().expect("remove_pid on missing file should succeed");
    }

    /// TestAtomicWrite: verify the PID file has content immediately after
    /// write_pid (the temp-file + rename strategy ensures no partial reads).
    #[test]
    fn write_pid_is_atomic() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = PidManager::new(dir.path());
        mgr.write_pid().expect("write_pid");

        // File should exist and contain the PID.
        let content = std::fs::read_to_string(mgr.pid_file()).expect("read PID file");
        let pid: u32 = content.trim().parse().expect("parse PID");
        assert_eq!(pid, std::process::id());
    }
}
