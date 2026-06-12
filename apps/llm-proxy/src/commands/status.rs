//! `status` command implementation.
//!
//! Checks if the proxy server is running by reading the PID file.

use anyhow::Result;
use llm_proxy_core::load_app_config;

use crate::paths::default_config_path;
use crate::pid::{is_process_running, read_pid};

/// Run the `status` command.
///
/// Note: The listen address is read from the default config path. If the server
/// was started with `--config` or `LLM_PROXY_CONFIG` pointing to a different
/// file, the displayed address may not match the actual bind address.
pub fn cmd_status() -> Result<()> {
    match read_pid()? {
        Some(pid) => {
            if is_process_running(pid) {
                println!("server is running (PID {pid})");
                // Try to load actual bind address from config.
                let config_path = default_config_path();
                if config_path.exists() {
                    if let Ok(cfg) = load_app_config(&config_path) {
                        println!("  listen: {}", cfg.server.bind);
                    } else {
                        println!("  listen: (could not parse config)");
                    }
                } else {
                    println!("  listen: (config not found: {})", config_path.display());
                }
                println!("  config: {}", config_path.display());
            } else {
                println!("server not running (stale PID {pid})");
            }
            Ok(())
        }
        None => {
            println!("server not running (no PID file)");
            Ok(())
        }
    }
}
