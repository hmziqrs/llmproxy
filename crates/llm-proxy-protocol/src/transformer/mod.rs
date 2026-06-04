//! Request and response transformation between API formats.
//!
//! Converts between Anthropic Messages API, OpenAI Chat Completions API,
//! OpenAI Responses API, and Google Gemini API request/response formats.

pub mod request;
pub mod response;
pub mod stream;
