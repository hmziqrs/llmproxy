//! Core types shared across all `llm-proxy` crates.
//!
//! Provides the server [`Config`] type and the crate-level
//! [`CoreError`] error enum.

#![deny(missing_docs)]

/// Configuration loading and types.
pub mod config;
/// Crate-level error type.
pub mod error;

pub use config::Config;
pub use error::CoreError;
