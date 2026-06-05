//! llm-proxy-provider: upstream provider client.
//!
//! Manages HTTP connections to upstream LLM providers (OpenCode Go and Zen),
//! with connection pooling, model-based endpoint routing, and streaming support.

pub mod client;
pub mod error;
pub mod sse;
pub mod transport;

pub use client::{
    EndpointType, OpenCodeClient, PROVIDER_OPENCODE_GO, PROVIDER_OPENCODE_ZEN,
    classify_endpoint, is_anthropic_model, is_gemini_model, is_responses_model, is_zen, provider,
};
pub use error::ProviderError;
pub use sse::{SseFrame, SseFramer};
pub use transport::{AuthHeaders, ProxyClient, ProxyRequest};
