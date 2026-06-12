//! `stop` command implementation.
//!
//! Stops a running proxy server by sending SIGTERM (graceful) then SIGKILL (forced).

use anyhow::{Result, bail};

use crate::pid::{STOP_MAX_POLLS, STOP_POLL_INTERVAL_MS, is_process_running, read_pid, remove_pid};

/// Run the `stop` command.
pub fn cmd_stop() -> Result<()> {
    #[cfg(not(unix))]
    {
        bail!("error: stop is not supported on this platform");
    }

    match read_pid()? {
        Some(pid) => {
            if !is_process_running(pid) {
                println!("server not running (stale PID {pid})");
                remove_pid()?;
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

            // Wait up to 10 seconds for graceful shutdown (STOP_MAX_POLLS x STOP_POLL_INTERVAL_MS).
            for _ in 0..STOP_MAX_POLLS {
                if !is_process_running(pid) {
                    remove_pid()?;
                    println!("server stopped");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(STOP_POLL_INTERVAL_MS));
            }

            // Force kill if still running after graceful period.
            println!(
                "WARNING: PID {pid} did not exit within {} seconds, escalating to SIGKILL",
                (STOP_MAX_POLLS * STOP_POLL_INTERVAL_MS as u32) / 1000
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
            remove_pid()?;
            println!("server force-stopped (PID {pid})");
            Ok(())
        }
        None => {
            println!("no PID file found -- server not running");
            Ok(())
        }
    }
}
