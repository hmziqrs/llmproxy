//! llm-proxy-protocol: wire-format types for the LLM proxy.
//!
//! Contains request/response types for the Anthropic Messages API,
//! OpenAI Chat Completions API, Responses API, Google Gemini API,
//! the transformer module that converts between these formats, and
//! the normalised core protocol types.

#![warn(missing_docs)]

pub mod anthropic;
pub mod core;
pub mod openai;
pub mod transformer;
pub mod zen;
