//! Integration tests for the `status` command.
//!
//! Verifies graceful handling when no PID file exists and when a stale PID
//! file is present. We cannot call `cmd_status()` directly because it reads
//! from the global config_dir, so we test the underlying pid helpers.

use llm_proxy_app::pid::is_process_running;

#[test]
fn status_no_pid_file_returns_none() {
    // read_pid() uses config_dir(), so we test the underlying PidManager
    // directly instead.
    let dir = tempfile::tempdir().unwrap();
    let mgr = llm_proxy_core::PidManager::new(dir.path());

    // No PID file → None.
    let pid = mgr.read_pid().unwrap();
    assert_eq!(pid, None);
}

#[test]
fn status_detects_current_process() {
    let my_pid = std::process::id();
    assert!(is_process_running(my_pid), "current process should be running");
}

#[test]
fn status_nonexistent_pid_not_running() {
    // PID 299999999 is extremely unlikely to exist.
    assert!(!is_process_running(299_999_999));
}

#[test]
fn status_pid_zero_not_running() {
    // PID 0 must not be reported as running to avoid signaling the entire
    // process group on Unix.
    assert!(!is_process_running(0));
}
