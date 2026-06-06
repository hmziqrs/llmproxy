//! llm-proxy-provider: upstream provider adapter framework.
//!
//! Manages protocol adapters for upstream LLM providers with connection pooling,
//! streaming support, and provider-specific encoding/decoding.

pub mod adapter;
pub mod error;
pub mod sse;
pub mod transport;

pub use adapter::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, ProviderAdapter, ProviderAdapterRegistry,
    ProviderAdapterTarget, ProviderProtocol, ProviderStreamDecoder, ResponsesAdapter,
};
pub use error::ProviderError;
pub use sse::{SseFrame, SseFramer};
pub use transport::{AuthHeaders, ProxyClient, ProxyRequest};
