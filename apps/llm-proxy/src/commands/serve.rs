//! `serve` command implementation.
//!
//! Starts the proxy server in foreground or background (daemon) mode.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};
use llm_proxy_server::{build_router, shutdown_signal};
use tokio::net::TcpListener;
use tracing::info;

use crate::config_validation::validate_toml_extension;
use crate::paths::{config_dir, resolve_config};
use crate::permissions::set_private_permissions;
use crate::pid::{is_process_running, read_pid, remove_pid, write_pid, write_pid_value};
use crate::state::{init_tracing, load_toml_state};

/// Run the `serve` command.
///
/// Requires TOML configuration.
pub async fn cmd_serve(
    config_path: Option<PathBuf>,
    port_override: Option<u16>,
    background: bool,
    daemonize: bool,
) -> Result<()> {
    // Initialize tracing before the daemonization check so that the parent
    // process can log during spawn_daemon(). The daemon child reinitializes
    // its own subscriber; calling init_tracing twice is safe (the second
    // call is a no-op since the global subscriber is already set).
    init_tracing();

    // If background mode requested, spawn self as child with --daemonize.
    if background && !daemonize {
        return spawn_daemon(config_path, port_override);
    }

    // Resolve config file path.
    let path = resolve_config(config_path.as_deref());

    validate_toml_extension(&path)?;

    // Build shared infrastructure.
    let adapter_registry = ProviderAdapterRegistry::builtin();
    let proxy_client = ProxyClient::new();

    // Load TOML config and build state before writing the PID file. This
    // ensures a bad config does not leave a stale PID file on disk.
    let state = load_toml_state(&path, adapter_registry, proxy_client, port_override)?;

    // Attempt atomic PID file creation. If it fails because the file already
    // exists, check whether the existing owner is still alive before removing
    // the stale file and retrying. This closes the TOCTOU window between the
    // stale-check and the write.
    if let Err(first_err) = write_pid() {
        // Atomic create_new failed -- file exists. Check if the owner is stale.
        if let Some(existing_pid) = read_pid()? {
            if is_process_running(existing_pid) {
                bail!(
                    "server already running with PID {} (PID file: {})",
                    existing_pid,
                    crate::paths::pid_file_path().display()
                );
            }
            info!("stale PID file found (PID {}), cleaning up", existing_pid);
        }
        remove_pid()?;
        // Retry -- this time the file is gone so create_new should succeed.
        write_pid().with_context(|| {
            format!(
                "failed to create PID file after removing stale entry: {}",
                first_err
            )
        })?;
    }

    // Ensure PID file is cleaned up on shutdown.
    let cleanup = async move {
        if let Err(e) = remove_pid() {
            tracing::warn!(error = %e, "failed to remove PID file during shutdown");
        }
    };

    let bind_addr = state.bind_address();
    let app = build_router(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding to {bind_addr}"))?;
    info!(
        addr = %listener.local_addr()?,
        version = env!("CARGO_PKG_VERSION"),
        "llm-proxy listening"
    );

    // Serve with graceful shutdown.
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;

    cleanup.await;

    match result {
        Ok(()) => {
            info!("server stopped cleanly");
            Ok(())
        }
        Err(e) => {
            tracing::error!(error = %e, "server error");
            Err(anyhow::anyhow!("server error: {}", e))
        }
    }
}

/// Spawn the current binary as a background daemon.
fn spawn_daemon(config_path: Option<PathBuf>, port_override: Option<u16>) -> Result<()> {
    let exe = std::env::current_exe().with_context(|| "resolving current executable")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--daemonize");

    if let Some(ref p) = config_path {
        cmd.arg("--config").arg(p);
    }
    if let Some(p) = port_override {
        cmd.arg("--port").arg(p.to_string());
    }

    // Detach from terminal.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        // Redirect stdout/stderr to a log file.
        // NOTE: The log file grows unbounded. For long-running daemons, set up
        // external log rotation (e.g., logrotate on Linux, newsyslog on macOS)
        // or pipe to a log aggregation service.
        let log_path = config_dir().join("llm-proxy.log");
        let log_file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening log file {}", log_path.display()))?;
        // Set restrictive permissions on the log file.
        set_private_permissions(&log_path)?;
        cmd.stdout(
            log_file
                .try_clone()
                .with_context(|| "cloning log file handle")?,
        );
        cmd.stderr(log_file);
    }

    #[cfg(not(unix))]
    {
        println!(
            "warning: daemon log redirection is not supported on this platform; \
                   output will be lost"
        );
    }

    let mut child = cmd.spawn().with_context(|| "spawning daemon process")?;

    let pid = child.id();

    // Write the PID file before detaching so callers can reliably find it.
    if let Err(e) = write_pid_value(pid) {
        tracing::warn!(error = %e, "failed to write PID file for daemon child");
    }

    // Confirm the child is still alive before reporting success.
    std::thread::sleep(std::time::Duration::from_millis(100));
    match child.try_wait() {
        Ok(Some(status)) => {
            bail!(
                "daemon child exited immediately with status {}",
                status.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string())
            );
        }
        Ok(None) => { /* child is running */ }
        Err(e) => {
            bail!("failed to check daemon child status: {}", e);
        }
    }

    println!("llm-proxy started in background (PID {pid})");
    println!("  config dir: {}", config_dir().display());
    println!(
        "  log file:   {}",
        config_dir().join("llm-proxy.log").display()
    );

    Ok(())
}
