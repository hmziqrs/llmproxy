//! llm-proxy-provider: upstream provider client.
//!
//! Manages HTTP connections to upstream LLM providers (OpenCode Go and Zen),
//! with connection pooling, model-based endpoint routing, and streaming support.

pub mod client;

pub use client::{
    EndpointType, OpenCodeClient, PROVIDER_OPENCODE_GO, PROVIDER_OPENCODE_ZEN, ProviderError,
    classify_endpoint, is_anthropic_model, is_gemini_model, is_responses_model, is_zen, provider,
};
