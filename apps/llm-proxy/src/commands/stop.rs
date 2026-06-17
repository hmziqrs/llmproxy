//! `stop` command implementation.
//!
//! Stops a running proxy server by sending SIGTERM (graceful) then SIGKILL (forced).
//!
//! The command logic is parameterised over a [`CommandPaths`] bundle so the
//! full escalation (stale cleanup, no-file, SIGTERM→poll→SIGKILL) can be
//! exercised against a tempdir-backed manager in tests (audit MEDIUM-6).

use std::time::Duration;

use anyhow::{Result, bail};

use crate::paths::CommandPaths;
use crate::pid::{STOP_MAX_POLLS, STOP_POLL_INTERVAL_MS, is_process_running, read_pid_from};

/// Run the `stop` command against the given paths using the production poll
/// cadence ([`STOP_MAX_POLLS`] × [`STOP_POLL_INTERVAL_MS`]).
///
/// Thin wrapper around [`cmd_stop_with_timing`] for the real CLI invocation.
pub fn cmd_stop(paths: &CommandPaths) -> Result<()> {
    cmd_stop_with_timing(
        paths,
        STOP_MAX_POLLS,
        Duration::from_millis(STOP_POLL_INTERVAL_MS),
    )
}

/// Run the `stop` command with an explicit graceful-shutdown poll cadence.
///
/// `max_polls` × `poll_interval` bounds how long the command waits for the
/// process to exit after SIGTERM before escalating to SIGKILL. The configurable
/// cadence exists so tests can drive the full escalation path quickly against a
/// tempdir-backed manager without waiting the production 10 s budget (audit
/// MEDIUM-6).
pub fn cmd_stop_with_timing(
    paths: &CommandPaths,
    max_polls: u32,
    poll_interval: Duration,
) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (max_polls, poll_interval);
        bail!("error: stop is not supported on this platform");
    }

    let mgr = paths.pid_manager();
    match read_pid_from(mgr)? {
        Some(pid) => {
            if !is_process_running(pid) {
                println!("server not running (stale PID {pid})");
                mgr.remove_pid()?;
                return Ok(());
            }

            #[cfg(unix)]
            {
                // Send SIGTERM.
                let ret = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                if ret != 0 {
                    let err = std::io::Error::last_os_error();
                    bail!(
                        "failed to send SIGTERM to PID {pid}: {} (os error {})",
                        err,
                        err.raw_os_error().unwrap_or(0)
                    );
                }
                println!("sent SIGTERM to PID {pid}");
            }

            // Wait up to `max_polls * poll_interval` for graceful shutdown.
            for _ in 0..max_polls {
                if !is_process_running(pid) {
                    mgr.remove_pid()?;
                    println!("server stopped");
                    return Ok(());
                }
                std::thread::sleep(poll_interval);
            }

            // Force kill if still running after graceful period.
            println!(
                "WARNING: PID {pid} did not exit within {} seconds, escalating to SIGKILL",
                max_polls as u64 * poll_interval.as_millis() as u64 / 1000
            );

            #[cfg(unix)]
            {
                let ret = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                if ret != 0 {
                    let err = std::io::Error::last_os_error();
                    bail!(
                        "failed to send SIGKILL to PID {pid}: {} (os error {})",
                        err,
                        err.raw_os_error().unwrap_or(0)
                    );
                }
            }
            mgr.remove_pid()?;
            println!("server force-stopped (PID {pid})");
            Ok(())
        }
        None => {
            println!("no PID file found -- server not running");
            Ok(())
        }
    }
}
