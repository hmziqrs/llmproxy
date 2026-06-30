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

/// Outcome of a best-effort re-read of the PID file immediately before
/// signaling. Used to narrow the TOCTOU / PID-reuse window in `cmd_stop`.
///
/// Only meaningful on Unix, where `cmd_stop` actually signals processes; gated
/// to match its `#[cfg(unix)]` call sites so non-Unix builds do not see a
/// dead-code warning.
#[cfg(unix)]
#[derive(Debug)]
enum PidOwnership {
    /// The PID file still holds exactly `expected`.
    StillOurs,
    /// The PID file now holds a different PID -- refuse to signal `expected`.
    Changed(u32),
    /// The PID file is gone, unreadable, or corrupt -- treat the process as no
    /// longer owned/tracked.
    Gone,
}

/// Re-read the PID file and classify it relative to `expected`.
///
/// Any error from `read_pid_from` (missing file, unreadable, corrupt contents,
/// invalid PID) is collapsed to [`PidOwnership::Gone`] rather than propagated,
/// because this is the *verification* read whose sole job is to decide whether
/// it is still safe to signal `expected`.
#[cfg(unix)]
fn classify_pid_ownership(mgr: &llm_proxy_core::PidManager, expected: u32) -> PidOwnership {
    match read_pid_from(mgr) {
        Ok(Some(current)) if current == expected => PidOwnership::StillOurs,
        Ok(Some(other)) => PidOwnership::Changed(other),
        Ok(None) => PidOwnership::Gone,
        Err(_) => PidOwnership::Gone,
    }
}

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
                // Narrow the TOCTOU / PID-reuse window before signaling.
                //
                // After the initial `read_pid_from` + `is_process_running` we may
                // have spent time doing I/O; re-read the PID file immediately
                // before sending the signal and refuse to proceed unless the file
                // still holds the *same* PID. If the daemon exited and cleaned up
                // its PID file (or the file was rewritten with a different PID),
                // we no longer own this PID and must not signal it.
                //
                // NOTE: this is NOT a full PID-identity check. `is_process_running`
                // only probes existence via `kill(pid, 0)`, and this re-read only
                // confirms the PID file still agrees with the PID we resolved. If
                // the OS recycled the *exact* PID for an unrelated process AND the
                // stale PID file was never updated, `stop` could still signal a
                // bystander. A robust fix needs platform-specific identity
                // verification (Linux `/proc/<pid>/comm`/`cmdline` or process
                // start-time; macOS `libproc` `proc_name`), which is intentionally
                // out of scope here to avoid pulling extra platform deps. The
                // re-read below removes the most common races (PID file cleared on
                // clean shutdown, or rewritten by a new `serve`).
                match classify_pid_ownership(mgr, pid) {
                    PidOwnership::StillOurs => { /* unchanged */ }
                    PidOwnership::Changed(other) => {
                        bail!(
                            "PID file changed while stopping (was {pid}, now {other}); \
                             refusing to signal {pid} to avoid hitting an unrelated process"
                        );
                    }
                    PidOwnership::Gone => {
                        println!(
                            "PID file disappeared before signaling; server likely already \
                             stopped (was PID {pid})"
                        );
                        return Ok(());
                    }
                }

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
                // Re-verify ownership immediately before the forceful SIGKILL,
                // which is the most dangerous signal: the polling loop above may
                // have run for several seconds, during which the PID file could
                // have been cleared or rewritten. See the SIGTERM block above for
                // the rationale and the residual PID-reuse caveat.
                match classify_pid_ownership(mgr, pid) {
                    PidOwnership::StillOurs => { /* unchanged */ }
                    PidOwnership::Changed(other) => {
                        bail!(
                            "PID file changed during graceful-shutdown wait \
                             (was {pid}, now {other}); refusing to SIGKILL {pid} to \
                             avoid hitting an unrelated process"
                        );
                    }
                    PidOwnership::Gone => {
                        // The process exited (and cleaned up its PID file) during
                        // the wait -- no escalation needed.
                        println!("server stopped during graceful-shutdown wait (PID {pid} gone)");
                        return Ok(());
                    }
                }
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
