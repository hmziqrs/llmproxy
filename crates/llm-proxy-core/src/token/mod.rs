//! Token counting utilities.
//!
//! Provides a [`Counter`] that counts tokens with a real BPE tokenizer
//! ([`tiktoken`]) for known OpenAI model ids and falls back to a character
//! heuristic (~4 chars/token) for everything else. The BPE ranks are bundled at
//! compile time, so counting is offline-safe and the heuristic is always
//! available as a last resort.

pub mod counter;
pub mod tiktoken;

pub use counter::{Counter, MessageContent};
