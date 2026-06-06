//! llm-proxy-protocol: wire-format types for the LLM proxy.
//!
//! Contains request/response types for the Anthropic Messages API,
//! OpenAI Chat Completions API, Responses API, Google Gemini API,
//! and the normalised core protocol types.

#![deny(missing_docs)]

pub mod anthropic;
pub mod client;
pub mod core;
pub mod openai;
pub mod zen;
