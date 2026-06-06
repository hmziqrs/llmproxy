//! llm-proxy-provider: upstream provider adapter framework.
//!
//! Manages protocol adapters for upstream LLM providers with connection pooling,
//! streaming support, and provider-specific encoding/decoding.

#![deny(missing_docs)]

/// Provider adapter implementations and registry.
pub mod adapter;
/// Provider error types and body sanitization.
pub mod error;
/// SSE framing for streaming provider responses.
pub mod sse;
/// Protocol-neutral HTTP transport with connection pooling.
pub mod transport;

pub use adapter::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, ProviderAdapter, ProviderAdapterRegistry,
    ProviderAdapterTarget, ProviderProtocol, ProviderStreamDecoder, ResponsesAdapter,
};
pub use error::ProviderError;
pub use sse::{SseFrame, SseFramer};
pub use transport::{AuthHeaders, ProxyClient, ProxyRequest};
