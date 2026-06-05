//! llm-proxy-protocol: wire-format types for the LLM proxy.
//!
//! Contains request/response types for the Anthropic Messages API,
//! OpenAI Chat Completions API, Responses API, Google Gemini API,
//! and the transformer module that converts between these formats.

pub mod anthropic;
pub mod openai;
pub mod transformer;
pub mod zen;
