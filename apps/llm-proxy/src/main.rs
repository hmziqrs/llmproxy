//! `llm-proxy` binary -- thin CLI dispatch.

use anyhow::Result;
use clap::Parser;
use llm_proxy_app::cli::{AutostartAction, Cli, Commands};
use llm_proxy_app::commands::*;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve {
            config,
            port,
            background,
            daemonize,
        } => cmd_serve(config, port, background, daemonize).await,
        Commands::Stop => cmd_stop(),
        Commands::Status => cmd_status(),
        Commands::Init => cmd_init(),
        Commands::Validate { config } => cmd_validate(config),
        Commands::Models {
            config,
            provider,
            live,
            write_catalog,
            require_success,
        } => cmd_models(config, provider, live, write_catalog, require_success).await,
        Commands::Autostart { action } => match action {
            AutostartAction::Enable { config, port } => cmd_autostart_enable(config, port),
            AutostartAction::Disable => cmd_autostart_disable(),
            AutostartAction::Status => cmd_autostart_status(),
        },
    }
}
