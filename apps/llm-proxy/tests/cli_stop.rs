//! Integration tests for the `stop` command.
//!
//! Verifies graceful handling when no PID file exists and that stale PID files
//! are detected. The actual signal-sending code requires a running process,
//! so we test the predicate logic only.

use llm_proxy_app::pid::is_process_running;

#[test]
fn stop_no_pid_file_is_ok() {
    // When there is no PID file, cmd_stop prints "no PID file found" and
    // returns Ok. We cannot call cmd_stop() directly because it reads the
    // PID file from the global config_dir. Instead verify the underlying
    // check: no file → read_pid returns None → stop returns Ok.
    let dir = tempfile::tempdir().unwrap();
    let mgr = llm_proxy_core::PidManager::new(dir.path());
    let pid = mgr.read_pid().unwrap();
    assert_eq!(pid, None, "no PID file should yield None");
}

#[test]
fn stop_stale_pid_detected() {
    // A PID that does not exist should return false from is_process_running.
    assert!(!is_process_running(299_999_999));
}

#[test]
fn stop_pid_zero_rejected() {
    // PID 0 must be treated as not running (would signal entire process group).
    assert!(!is_process_running(0));
}

#[test]
fn stop_current_process_detected() {
    let my_pid = std::process::id();
    assert!(is_process_running(my_pid));
}
