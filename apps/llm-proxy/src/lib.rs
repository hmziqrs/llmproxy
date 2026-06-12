//! `llm-proxy` application library.
//!
//! Multi-command CLI for the LLM proxy server. Uses TOML config exclusively.
//!
//! Commands:
//!
//! - `serve`    Start the proxy server (foreground or daemon)
//! - `stop`     Stop a running daemon
//! - `status`   Check if the server is running
//! - `init`     Create default TOML config and provider files
//! - `validate` Validate TOML config and print provider route table
//! - `models`   List provider-scoped models
//! - `autostart` Manage auto-start on login (enable / disable / status)

pub mod cli;
pub mod commands;
pub mod config_validation;
pub mod defaults;
pub mod paths;
pub mod permissions;
pub mod pid;
pub mod platform;
pub mod state;
