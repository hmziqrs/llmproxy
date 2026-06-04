//! llm-proxy-protocol: wire-format types for the LLM proxy.
//!
//! Contains request/response types for the OpenAI Chat Completions API,
//! Responses API, and Gemini API.

pub mod anthropic;
pub mod openai;
pub mod transformer;
pub mod zen;
