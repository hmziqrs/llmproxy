//! `llm-proxy` binary -- CLI entry point.
//!
//! Implements a multi-command CLI matching the Go `oc-go-cc` reference:
//!
//! - `serve`    Start the proxy server (foreground or daemon)
//! - `stop`     Stop a running daemon
//! - `status`   Check if the server is running
//! - `init`     Create default config file
//! - `validate` Validate a config file and print settings
//! - `models`   List available model IDs
//! - `autostart` Manage auto-start on login (enable / disable / status)

use std::io::Write as IoWrite;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use llm_proxy_core::{Config, FallbackHandler};
use llm_proxy_provider::OpenCodeClient;
use llm_proxy_server::{AppState, BuildInfo, build_router, shutdown_signal};
use tokio::net::TcpListener;
use tracing::info;

// ---------------------------------------------------------------------------
// CLI definitions
// ---------------------------------------------------------------------------

/// LLM proxy server.
#[derive(Parser, Debug)]
#[command(
    name = "llm-proxy",
    version,
    about = "LLM proxy server with scenario-based routing",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the proxy server.
    Serve {
        /// Path to config file.
        #[arg(short, long, env = "OC_GO_CC_CONFIG")]
        config: Option<PathBuf>,
        /// Override listen port.
        #[arg(short, long)]
        port: Option<u16>,
        /// Run as background daemon.
        #[arg(short, long)]
        background: bool,
        /// Internal: used by daemon child process.
        #[arg(long, hide = true)]
        _daemonize: bool,
    },
    /// Stop a running server.
    Stop,
    /// Check if the server is running.
    Status,
    /// Create a default config file.
    Init,
    /// Validate a config file and print settings.
    Validate {
        /// Path to config file.
        #[arg(short, long, env = "OC_GO_CC_CONFIG")]
        config: Option<PathBuf>,
    },
    /// List available model IDs.
    Models,
    /// Manage auto-start on login.
    Autostart {
        #[command(subcommand)]
        action: AutostartAction,
    },
}

#[derive(Subcommand, Debug)]
enum AutostartAction {
    /// Enable auto-start.
    Enable {
        /// Path to config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Port to listen on.
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Disable auto-start.
    Disable,
    /// Show auto-start status.
    Status,
}

// ---------------------------------------------------------------------------
// Default config
// ---------------------------------------------------------------------------

/// Default config JSON matching the Go reference defaults.
const DEFAULT_CONFIG: &str = r#"{
  "api_key": "",
  "host": "127.0.0.1",
  "port": 3456,
  "hot_reload": false,
  "enable_streaming_scenario_routing": true,
  "respect_requested_model": false,
  "models": {
    "default": {
      "provider": "opencode-go",
      "model_id": "kimi-k2.6",
      "temperature": 0.7,
      "max_tokens": 4096
    },
    "think": {
      "provider": "opencode-go",
      "model_id": "glm-5",
      "temperature": 0.7,
      "max_tokens": 8192,
      "reasoning_effort": "high"
    },
    "complex": {
      "provider": "opencode-go",
      "model_id": "glm-5.1",
      "temperature": 0.7,
      "max_tokens": 16384,
      "reasoning_effort": "max"
    },
    "background": {
      "provider": "opencode-go",
      "model_id": "qwen3.5-plus",
      "temperature": 0.5,
      "max_tokens": 2048
    },
    "long_context": {
      "provider": "opencode-go",
      "model_id": "minimax-m2.5",
      "temperature": 0.7,
      "max_tokens": 4096,
      "context_threshold": 100000
    }
  },
  "fallbacks": {
    "default": [
      { "provider": "opencode-go", "model_id": "qwen3.6-plus" },
      { "provider": "opencode-go", "model_id": "glm-5.1" }
    ],
    "think": [
      { "provider": "opencode-go", "model_id": "kimi-k2.6" },
      { "provider": "opencode-go", "model_id": "deepseek-v4-pro" }
    ],
    "complex": [
      { "provider": "opencode-go", "model_id": "kimi-k2.6" },
      { "provider": "opencode-go", "model_id": "deepseek-v4-pro" }
    ]
  },
  "opencode_go": {
    "base_url": "https://opencode.ai/zen/go/v1/chat/completions",
    "anthropic_base_url": "https://opencode.ai/zen/go/v1/messages",
    "timeout_ms": 300000
  },
  "opencode_zen": {
    "base_url": "https://opencode.ai/zen/v1/chat/completions",
    "anthropic_base_url": "https://opencode.ai/zen/v1/messages",
    "responses_base_url": "https://opencode.ai/zen/v1/responses",
    "gemini_base_url": "https://opencode.ai/zen/v1/models",
    "timeout_ms": 300000
  },
  "logging": {
    "level": "info",
    "requests": false
  }
}"#;

// ---------------------------------------------------------------------------
// Known model IDs (hardcoded from Go reference)
// ---------------------------------------------------------------------------

const MODEL_IDS: &[&str] = &[
    // OpenCode Go models
    "kimi-k2.6",
    "glm-5",
    "glm-5.1",
    "qwen3.5-plus",
    "qwen3.6-plus",
    "qwen3.7-max",
    "deepseek-v4-pro",
    "minimax-m2.5",
    "minimax-m2.7",
    // OpenCode Zen models
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.4",
    "gpt-5.4-pro",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
    "gpt-5",
    "gpt-5-codex",
    "gpt-5-nano",
    "gemini-3.5-flash",
    "gemini-3.1-pro",
    "gemini-3-flash",
    "claude-sonnet-4-20250514",
    "claude-opus-4-20250116",
];

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Config directory: `~/.config/llm-proxy/`.
fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("llm-proxy")
}

/// Config file path: `~/.config/llm-proxy/config.json`.
fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// PID file path: `~/.config/llm-proxy/llm-proxy.pid`.
fn pid_file_path() -> PathBuf {
    config_dir().join("llm-proxy.pid")
}

/// Resolve the config file path from CLI arg, env var, or default.
fn resolve_config(cli_path: Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli_path {
        return p;
    }
    if let Ok(p) = std::env::var("OC_GO_CC_CONFIG") {
        return PathBuf::from(p);
    }
    config_path()
}

// ---------------------------------------------------------------------------
// PID file helpers
// ---------------------------------------------------------------------------

/// Read the PID from the PID file. Returns `None` if the file does not exist.
fn read_pid() -> Result<Option<u32>> {
    let path = pid_file_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading PID file {}", path.display()))?;
    let pid: u32 = content
        .trim()
        .parse()
        .with_context(|| format!("parsing PID from {}", path.display()))?;
    Ok(Some(pid))
}

/// Write the current PID to the PID file.
fn write_pid() -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;
    let path = pid_file_path();
    let pid = std::process::id();
    let mut f = std::fs::File::create(&path)
        .with_context(|| format!("creating PID file {}", path.display()))?;
    write!(f, "{pid}").with_context(|| format!("writing PID file {}", path.display()))?;
    info!(pid, "wrote PID file");
    Ok(())
}

/// Remove the PID file.
fn remove_pid() -> Result<()> {
    let path = pid_file_path();
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("removing PID file {}", path.display()))?;
    }
    Ok(())
}

/// Check if a process with the given PID is running.
fn is_process_running(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) just checks if the process exists; it does not
    // send a signal on any Unix platform.
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Fallback: always assume running on non-Unix.
        let _ = pid;
        true
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            config,
            port,
            background,
            _daemonize,
        } => cmd_serve(config, port, background, _daemonize).await,
        Commands::Stop => cmd_stop(),
        Commands::Status => cmd_status(),
        Commands::Init => cmd_init(),
        Commands::Validate { config } => cmd_validate(config),
        Commands::Models => cmd_models(),
        Commands::Autostart { action } => match action {
            AutostartAction::Enable { config, port } => cmd_autostart_enable(config, port),
            AutostartAction::Disable => cmd_autostart_disable(),
            AutostartAction::Status => cmd_autostart_status(),
        },
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

/// Run the `serve` command.
async fn cmd_serve(
    config_path: Option<PathBuf>,
    port_override: Option<u16>,
    background: bool,
    daemonize: bool,
) -> Result<()> {
    // If background mode requested, spawn self as child with --_daemonize.
    if background && !daemonize {
        return spawn_daemon(config_path, port_override);
    }

    init_tracing();

    // Load config.
    let path = resolve_config(config_path);
    let config = if path.exists() {
        Config::load(&path).with_context(|| format!("loading config from {}", path.display()))?
    } else {
        info!("no config file found, using defaults");
        let mut cfg = Config::default();
        // Allow env var overrides even without config file.
        if let Ok(key) = std::env::var("OC_GO_CC_API_KEY") {
            cfg.api_key = key;
        }
        cfg
    };

    // Apply CLI port override.
    let mut config = config;
    if let Some(p) = port_override {
        config.port = p;
    }

    // Sync legacy bind field.
    let bind_addr: std::net::SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", config.host, config.port))?;
    config.bind = bind_addr;

    // Check if already running.
    if let Some(pid) = read_pid()? {
        if is_process_running(pid) {
            bail!(
                "server already running with PID {} (PID file: {})",
                pid,
                pid_file_path().display()
            );
        }
        info!("stale PID file found, cleaning up");
        remove_pid()?;
    }

    // Write PID file.
    write_pid()?;

    // Ensure PID file is cleaned up on shutdown.
    let pid_path = pid_file_path();
    let cleanup = async move {
        let _ = std::fs::remove_file(&pid_path);
    };

    // Build state and router.
    let config_arc = Arc::new(config);
    let client = OpenCodeClient::new(Arc::clone(&config_arc));
    let fallback_handler = FallbackHandler::new(3, std::time::Duration::from_secs(30));
    let state = AppState::new(
        Arc::try_unwrap(config_arc).unwrap_or_else(|arc| (*arc).clone()),
        build_info(),
        client,
        fallback_handler,
    );
    let app = build_router(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding to {bind_addr}"))?;
    info!(
        addr = %listener.local_addr()?,
        version = env!("CARGO_PKG_VERSION"),
        "llm-proxy listening"
    );

    // Start config watcher if hot_reload enabled.
    // TODO: implement file watcher in a future wave.

    // Serve with graceful shutdown.
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    cleanup.await;

    match result {
        Ok(()) => info!("server stopped cleanly"),
        Err(e) => tracing::error!(error = %e, "server error"),
    }

    Ok(())
}

/// Spawn the current binary as a background daemon.
fn spawn_daemon(config_path: Option<PathBuf>, port_override: Option<u16>) -> Result<()> {
    let exe = std::env::current_exe().with_context(|| "resolving current executable")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--_daemonize");

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
        let log_path = config_dir().join("llm-proxy.log");
        let log_file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening log file {}", log_path.display()))?;
        cmd.stdout(
            log_file
                .try_clone()
                .with_context(|| "cloning log file handle")?,
        );
        cmd.stderr(log_file);
    }

    let mut child = cmd.spawn().with_context(|| "spawning daemon process")?;

    let pid = child.id();
    println!("llm-proxy started in background (PID {pid})");
    println!("  config dir: {}", config_dir().display());
    println!(
        "  log file:   {}",
        config_dir().join("llm-proxy.log").display()
    );

    // Detach from child so we don't wait on it.
    // On Unix, `process_group(0)` already detached it.
    let _ = child.try_wait();

    Ok(())
}

/// Run the `stop` command.
fn cmd_stop() -> Result<()> {
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
                    bail!("failed to send SIGTERM to PID {pid}");
                }
            }
            #[cfg(not(unix))]
            {
                // Non-Unix: just remove the PID file.
            }

            println!("sent SIGTERM to PID {pid}");

            // Wait briefly for process to exit.
            for _ in 0..10 {
                if !is_process_running(pid) {
                    remove_pid()?;
                    println!("server stopped");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }

            // Force kill if still running.
            #[cfg(unix)]
            {
                let ret = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                if ret != 0 {
                    bail!("failed to send SIGKILL to PID {pid}");
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

/// Run the `status` command.
fn cmd_status() -> Result<()> {
    match read_pid()? {
        Some(pid) => {
            if is_process_running(pid) {
                println!("server is running (PID {pid})");
                println!("  listen: 127.0.0.1:3456");
                println!("  config: {}", config_path().display());
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

/// Run the `init` command.
fn cmd_init() -> Result<()> {
    let path = config_path();
    if path.exists() {
        bail!("config file already exists at {}", path.display());
    }

    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;

    // Write default config.
    let mut f =
        std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(DEFAULT_CONFIG.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;

    println!("created default config at {}", path.display());
    println!("edit it to add your API key (or set OC_GO_CC_API_KEY env var)");
    Ok(())
}

/// Run the `validate` command.
fn cmd_validate(config_path: Option<PathBuf>) -> Result<()> {
    let path = resolve_config(config_path);

    println!("validating config: {}", path.display());

    let config =
        Config::load(&path).with_context(|| format!("loading config from {}", path.display()))?;

    // Print settings summary.
    println!();
    println!("=== Configuration ===");
    println!("  host:              {}", config.host);
    println!("  port:              {}", config.port);
    println!(
        "  api_key:           {}...",
        &config.api_key[..config.api_key.len().min(8)]
    );
    println!("  hot_reload:        {}", config.hot_reload);
    println!(
        "  streaming routing: {}",
        config.enable_streaming_scenario_routing
    );
    println!("  respect model:     {}", config.respect_requested_model);

    println!();
    println!("=== Models ===");
    for (scenario, model) in &config.models {
        println!(
            "  [{}] provider={} model={} temp={} max_tokens={}",
            scenario, model.provider, model.model_id, model.temperature, model.max_tokens
        );
        if !model.reasoning_effort.is_empty() {
            println!("     reasoning_effort={}", model.reasoning_effort);
        }
        if let Some(thinking) = &model.thinking {
            println!("     thinking={thinking}");
        }
        if model.context_threshold > 0 {
            println!("     context_threshold={}", model.context_threshold);
        }
    }

    println!();
    println!("=== Fallbacks ===");
    if config.fallbacks.is_empty() {
        println!("  (none)");
    } else {
        for (scenario, fallbacks) in &config.fallbacks {
            println!("  [{}]", scenario);
            for fb in fallbacks {
                println!("    - provider={} model={}", fb.provider, fb.model_id);
            }
        }
    }

    println!();
    println!("=== OpenCode Go ===");
    println!("  base_url:           {}", config.opencode_go.base_url);
    println!(
        "  anthropic_base_url: {}",
        config.opencode_go.anthropic_base_url
    );
    println!("  timeout_ms:         {}", config.opencode_go.timeout_ms);

    println!();
    println!("=== OpenCode Zen ===");
    println!("  base_url:           {}", config.opencode_zen.base_url);
    println!(
        "  anthropic_base_url: {}",
        config.opencode_zen.anthropic_base_url
    );
    println!(
        "  responses_base_url: {}",
        config.opencode_zen.responses_base_url
    );
    println!(
        "  gemini_base_url:    {}",
        config.opencode_zen.gemini_base_url
    );
    println!("  timeout_ms:         {}", config.opencode_zen.timeout_ms);

    println!();
    println!("=== Logging ===");
    println!("  level:    {}", config.logging.level);
    println!("  requests: {}", config.logging.requests);

    println!();
    println!("Config is valid!");
    Ok(())
}

/// Run the `models` command.
fn cmd_models() -> Result<()> {
    println!("Available model IDs:");
    println!();
    println!("--- OpenCode Go ---");
    for id in MODEL_IDS.iter().filter(|id| !is_zen_model(id)) {
        println!("  {id}");
    }
    println!();
    println!("--- OpenCode Zen ---");
    for id in MODEL_IDS.iter().filter(|id| is_zen_model(id)) {
        println!("  {id}");
    }
    Ok(())
}

/// Check if a model ID is served by the Zen provider.
fn is_zen_model(id: &&str) -> bool {
    const ZEN_MODELS: &[&str] = &[
        "gpt-5.5",
        "gpt-5.5-pro",
        "gpt-5.4",
        "gpt-5.4-pro",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.3-codex",
        "gpt-5.3-codex-spark",
        "gpt-5.2",
        "gpt-5.2-codex",
        "gpt-5.1",
        "gpt-5.1-codex",
        "gpt-5.1-codex-max",
        "gpt-5.1-codex-mini",
        "gpt-5",
        "gpt-5-codex",
        "gpt-5-nano",
        "gemini-3.5-flash",
        "gemini-3.1-pro",
        "gemini-3-flash",
        "claude-sonnet-4-20250514",
        "claude-opus-4-20250116",
    ];
    ZEN_MODELS.contains(id)
}

/// Run the `autostart enable` command.
fn cmd_autostart_enable(config_path: Option<PathBuf>, port: Option<u16>) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;

    let exe = std::env::current_exe().with_context(|| "resolving current executable")?;
    let mut args = vec![exe.to_string_lossy().to_string(), "serve".to_string()];

    if let Some(ref p) = config_path {
        args.push("--config".to_string());
        args.push(p.to_string_lossy().to_string());
    }
    if let Some(p) = port {
        args.push("--port".to_string());
        args.push(p.to_string());
    }

    #[cfg(target_os = "macos")]
    {
        let plist_content = format_plist(&args.join(" "));
        let plist_path = dirs::home_dir()
            .map(|h| {
                h.join("Library")
                    .join("LaunchAgents")
                    .join("com.llm-proxy.plist")
            })
            .unwrap_or_else(|| PathBuf::from("com.llm-proxy.plist"));

        let plist_dir = plist_path.parent().unwrap();
        std::fs::create_dir_all(plist_dir)
            .with_context(|| format!("creating {}", plist_dir.display()))?;

        std::fs::write(&plist_path, plist_content)
            .with_context(|| format!("writing {}", plist_path.display()))?;

        println!("created launchd plist at {}", plist_path.display());
        println!("run: launchctl load {}", plist_path.display());
    }

    #[cfg(target_os = "linux")]
    {
        let desktop_content = format_desktop_entry(&args.join(" "));
        let autostart_dir = dirs::home_dir()
            .map(|h| h.join(".config").join("autostart"))
            .unwrap_or_else(|| PathBuf::from(".config/autostart"));
        std::fs::create_dir_all(&autostart_dir)
            .with_context(|| format!("creating {}", autostart_dir.display()))?;
        let desktop_path = autostart_dir.join("llm-proxy.desktop");
        std::fs::write(&desktop_path, desktop_content)
            .with_context(|| format!("writing {}", desktop_path.display()))?;
        println!("created desktop entry at {}", desktop_path.display());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("auto-start is not supported on this platform");
        println!("add this command to your shell startup script:");
        println!("  {}", args.join(" "));
    }

    Ok(())
}

/// Run the `autostart disable` command.
fn cmd_autostart_disable() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .map(|h| {
                h.join("Library")
                    .join("LaunchAgents")
                    .join("com.llm-proxy.plist")
            })
            .unwrap_or_else(|| PathBuf::from("com.llm-proxy.plist"));

        if plist_path.exists() {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &plist_path.to_string_lossy()])
                .output();
            std::fs::remove_file(&plist_path)
                .with_context(|| format!("removing {}", plist_path.display()))?;
            println!("removed launchd plist");
        } else {
            println!("no launchd plist found");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let autostart_dir = dirs::home_dir()
            .map(|h| h.join(".config").join("autostart"))
            .unwrap_or_else(|| PathBuf::from(".config/autostart"));
        let desktop_path = autostart_dir.join("llm-proxy.desktop");
        if desktop_path.exists() {
            std::fs::remove_file(&desktop_path)
                .with_context(|| format!("removing {}", desktop_path.display()))?;
            println!("removed desktop entry");
        } else {
            println!("no desktop entry found");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("auto-start is not supported on this platform");
    }

    Ok(())
}

/// Run the `autostart status` command.
fn cmd_autostart_status() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .map(|h| {
                h.join("Library")
                    .join("LaunchAgents")
                    .join("com.llm-proxy.plist")
            })
            .unwrap_or_else(|| PathBuf::from("com.llm-proxy.plist"));

        if plist_path.exists() {
            println!("auto-start is enabled ({})", plist_path.display());
        } else {
            println!("auto-start is disabled (no launchd plist found)");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let autostart_dir = dirs::home_dir()
            .map(|h| h.join(".config").join("autostart"))
            .unwrap_or_else(|| PathBuf::from(".config/autostart"));
        let desktop_path = autostart_dir.join("llm-proxy.desktop");
        if desktop_path.exists() {
            println!("auto-start is enabled ({})", desktop_path.display());
        } else {
            println!("auto-start is disabled (no desktop entry found)");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("auto-start is not supported on this platform");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Platform-specific helpers
// ---------------------------------------------------------------------------

/// Generate a macOS launchd plist for auto-start.
#[cfg(target_os = "macos")]
fn format_plist(command: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.llm-proxy</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>-c</string>
        <string>{command}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{log_path}</string>
    <key>StandardErrorPath</key>
    <string>{log_path}</string>
</dict>
</plist>
"#,
        log_path = config_dir().join("llm-proxy.log").display()
    )
}

/// Generate a Linux XDG desktop entry for auto-start.
#[cfg(target_os = "linux")]
fn format_desktop_entry(command: &str) -> String {
    format!(
        r#"[Desktop Entry]
Type=Application
Name=LLM Proxy
Exec={command}
X-GNOME-Autostart-enabled=true
Hidden=false
"#
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// dirs helper (minimal, no external dep)
// ---------------------------------------------------------------------------

mod dirs {
    use std::path::PathBuf;

    /// Returns the user's home directory.
    pub fn home_dir() -> Option<PathBuf> {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(PathBuf::from)
    }
}
