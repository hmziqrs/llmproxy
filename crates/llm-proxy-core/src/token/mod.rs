//! Token counting utilities.
//!
//! Provides a simplified token counter that estimates token usage based on
//! character heuristics. This is **not** a full tiktoken-compatible
//! implementation — it uses a rough approximation of ~4 characters per token.

pub mod counter;

pub use counter::{Counter, MessageContent};
