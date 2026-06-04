//! `llm-proxy` binary entry point.
//!
//! Boots an axum HTTP server with ops routes and an echo chat endpoint.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use llm_proxy_core::Config;
use llm_proxy_server::{AppState, BuildInfo, build_router, shutdown_signal};
use tokio::net::TcpListener;
use tracing::info;

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(name = "llm-proxy", version, about)]
struct Cli {
    /// Path to a TOML config file. Falls back to `$LLM_PROXY_CONFIG`,
    /// then to built-in defaults.
    #[arg(long, env = "LLM_PROXY_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config = match cli.config {
        Some(p) => {
            Config::load(&p).with_context(|| format!("loading config from {}", p.display()))?
        }
        None => Config::default(),
    };

    let bind = config.bind;
    let state = AppState::new(config, build_info());
    let app = build_router(state);

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding to {bind}"))?;
    info!(addr = %listener.local_addr()?, "llm-proxy listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    info!("server stopped cleanly");
    Ok(())
}

fn build_info() -> BuildInfo {
    BuildInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        target: env!("VERGEN_CARGO_TARGET_TRIPLE"),
        git_sha: env!("VERGEN_GIT_SHA"),
    }
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
