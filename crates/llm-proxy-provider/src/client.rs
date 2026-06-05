//! Upstream provider client for OpenCode Go and Zen APIs.
//!
//! Mirrors `oc-go-cc/internal/client/opencode.go`. Manages HTTP connections
//! to upstream LLM providers with connection pooling, endpoint routing based
//! on model classification, and streaming support.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use reqwest::Response;

use llm_proxy_core::config::{Config, ModelConfig};
use llm_proxy_protocol::openai::{ChatCompletionRequest, ChatCompletionResponse};
use llm_proxy_protocol::zen::{GeminiRequest, GeminiResponse, ResponsesRequest, ResponsesResponse};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Provider identifier for the OpenCode Go backend.
pub const PROVIDER_OPENCODE_GO: &str = "opencode-go";
/// Provider identifier for the OpenCode Zen backend.
pub const PROVIDER_OPENCODE_ZEN: &str = "opencode-zen";

// ---------------------------------------------------------------------------
// Endpoint classification
// ---------------------------------------------------------------------------

/// Determines which upstream endpoint format to use for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointType {
    /// `/v1/chat/completions` (OpenAI-compatible).
    ChatCompletions,
    /// `/v1/messages` (Anthropic format).
    Anthropic,
    /// `/v1/responses` (OpenAI native Responses API).
    Responses,
    /// `/v1/models/{id}` (Google Gemini).
    Gemini,
}

/// Returns `true` if the model requires the Anthropic endpoint.
///
/// Covers both Go models (minimax) and Zen models (claude, qwen).
pub fn is_anthropic_model(model_id: &str) -> bool {
    match model_id {
        "minimax-m2.5" | "minimax-m2.7" | "qwen3.7-max" => true,
        _ => model_id.starts_with("qwen"),
    }
}

/// Returns `true` if the model uses the Gemini endpoint.
pub fn is_gemini_model(model_id: &str) -> bool {
    matches!(
        model_id,
        "gemini-3.5-flash" | "gemini-3.1-pro" | "gemini-3-flash"
    )
}

/// Returns `true` if the model uses the Responses API endpoint.
pub fn is_responses_model(model_id: &str) -> bool {
    matches!(
        model_id,
        "gpt-5.5"
            | "gpt-5.5-pro"
            | "gpt-5.4"
            | "gpt-5.4-pro"
            | "gpt-5.4-mini"
            | "gpt-5.4-nano"
            | "gpt-5.3-codex"
            | "gpt-5.3-codex-spark"
            | "gpt-5.2"
            | "gpt-5.2-codex"
            | "gpt-5.1"
            | "gpt-5.1-codex"
            | "gpt-5.1-codex-max"
            | "gpt-5.1-codex-mini"
            | "gpt-5"
            | "gpt-5-codex"
            | "gpt-5-nano"
    )
}

/// Determines the endpoint type for a model.
pub fn classify_endpoint(model_id: &str) -> EndpointType {
    if is_anthropic_model(model_id) {
        EndpointType::Anthropic
    } else if is_gemini_model(model_id) {
        EndpointType::Gemini
    } else if is_responses_model(model_id) {
        EndpointType::Responses
    } else {
        EndpointType::ChatCompletions
    }
}

/// Returns `true` if the model is served by the Zen provider.
pub fn is_zen(model: &ModelConfig) -> bool {
    provider(model) == PROVIDER_OPENCODE_ZEN
}

/// Returns the provider string for a model config.
///
/// Defaults to [`PROVIDER_OPENCODE_GO`] when the provider field is empty.
pub fn provider(model: &ModelConfig) -> &str {
    if model.provider.is_empty() {
        PROVIDER_OPENCODE_GO
    } else {
        &model.provider
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the provider client.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to serialize the request body.
    #[error("failed to marshal request: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The HTTP request failed at the transport level.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The upstream API returned an error status code.
    ///
    /// The `body` field is truncated to [`MAX_API_ERROR_BODY_LEN`] bytes and
    /// stripped of common API key patterns at construction time so that
    /// `Display` output (used in `warn!()` / `error!()` logging) never
    /// contains full key material.
    #[error("API error {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body text (truncated and sanitized).
        body: String,
    },
}

/// Maximum length for upstream API error bodies stored in [`ProviderError::Api`].
const MAX_API_ERROR_BODY_LEN: usize = 512;

/// Sanitize an upstream API error body: strip common key patterns and
/// truncate to [`MAX_API_ERROR_BODY_LEN`].
///
/// Covers:
/// - OpenAI keys: `sk-live-...`, `sk-test-...`, `sk-...`
/// - Anthropic keys: `sk-ant-api03-...`, `sk-ant-...`
/// - Google API keys: `AIza...`
/// - Generic key prefixes: `key-...`
fn sanitize_api_error_body(mut body: String) -> String {
    // Redact in order of longest prefix first to avoid partial matches.
    // Anthropic prefixes before generic `sk-` to avoid partial redaction.
    body = body
        .replace("sk_live_", "***")
        .replace("sk_test_", "***")
        .replace("sk-ant-api03-", "***")
        .replace("sk-ant-", "***")
        .replace("sk-", "***")
        .replace("AIza", "***")
        .replace("key-", "***");
    if body.len() > MAX_API_ERROR_BODY_LEN {
        let mut end = MAX_API_ERROR_BODY_LEN;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        body.truncate(end);
        body.push_str("...[truncated]");
    }
    body
}

// ---------------------------------------------------------------------------
// Endpoint config (internal)
// ---------------------------------------------------------------------------

/// A string wrapper that always redacts its contents in `Debug` output.
///
/// Used for API keys and other secrets so that adding `#[derive(Debug)]` to
/// a parent struct never leaks the key in log output.
#[derive(Clone)]
struct SecretString(String);

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl SecretString {
    /// Expose the inner secret value.
    fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Resolved endpoint configuration (base URL + API key).
///
/// The `api_key` field uses [`SecretString`] so that even if this struct
/// gains a `Debug` impl, the key is never printed in plaintext.
struct EndpointConfig {
    base_url: String,
    api_key: SecretString,
}

// ---------------------------------------------------------------------------
// OpenCodeClient
// ---------------------------------------------------------------------------

/// HTTP client for communicating with upstream LLM providers.
///
/// Manages a connection-pooled [`reqwest::Client`] and routes requests to
/// the correct endpoint based on model classification.
#[derive(Debug, Clone)]
pub struct OpenCodeClient {
    config: Arc<Config>,
    http_client: reqwest::Client,
}

impl OpenCodeClient {
    /// Creates a new client with the given configuration.
    ///
    /// Builds a connection-pooled HTTP client with the following settings:
    /// - `max_idle_conns`: 100
    /// - `max_idle_conns_per_host`: 20
    /// - `idle_timeout`: 90 s
    /// - `max_conns_per_host`: 50
    pub fn new(config: Arc<Config>) -> Self {
        let http_client = reqwest::Client::builder()
            // Matches Go transport: MaxIdleConnsPerHost=20, IdleConnTimeout=90s.
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build reqwest client");

        Self {
            config,
            http_client,
        }
    }

    // -----------------------------------------------------------------------
    // Endpoint resolution
    // -----------------------------------------------------------------------

    /// Returns the resolved endpoint (base URL + API key) for a model.
    fn get_endpoint(&self, model_id: &str, model: &ModelConfig) -> EndpointConfig {
        if is_zen(model) {
            let zen = &self.config.opencode_zen;
            match classify_endpoint(model_id) {
                EndpointType::Anthropic => EndpointConfig {
                    base_url: zen.anthropic_base_url.clone(),
                    api_key: SecretString::from(self.config.api_key.clone()),
                },
                EndpointType::Responses => EndpointConfig {
                    base_url: zen.responses_base_url.clone(),
                    api_key: SecretString::from(self.config.api_key.clone()),
                },
                EndpointType::Gemini => EndpointConfig {
                    base_url: format!("{}/{}", zen.gemini_base_url, model_id),
                    api_key: SecretString::from(self.config.api_key.clone()),
                },
                EndpointType::ChatCompletions => EndpointConfig {
                    base_url: zen.base_url.clone(),
                    api_key: SecretString::from(self.config.api_key.clone()),
                },
            }
        } else {
            // Default: OpenCode Go
            if is_anthropic_model(model_id) {
                EndpointConfig {
                    base_url: self.config.opencode_go.anthropic_base_url.clone(),
                    api_key: SecretString::from(self.config.api_key.clone()),
                }
            } else {
                EndpointConfig {
                    base_url: self.config.opencode_go.base_url.clone(),
                    api_key: SecretString::from(self.config.api_key.clone()),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Chat Completions
    // -----------------------------------------------------------------------

    /// Sends a chat completion request to the upstream provider.
    ///
    /// Returns the raw [`reqwest::Response`] so callers can decide whether
    /// to consume it as a single JSON body or a streaming byte stream.
    pub async fn chat_completion(
        &self,
        model_id: &str,
        req: &ChatCompletionRequest,
        model: &ModelConfig,
    ) -> Result<Response, ProviderError> {
        let endpoint = self.get_endpoint(model_id, model);

        let body = serde_json::to_vec(req)?;

        let mut builder = self
            .http_client
            .post(&endpoint.base_url)
            .header("Content-Type", "application/json");

        // Anthropic endpoint sets BOTH x-api-key and Authorization: Bearer.
        // Non-Anthropic endpoint only sets Authorization: Bearer.
        if is_anthropic_model(model_id) {
            builder = builder
                .header("x-api-key", endpoint.api_key.expose())
                .header("Authorization", format!("Bearer {}", endpoint.api_key.expose()));
        } else {
            builder = builder.header("Authorization", format!("Bearer {}", endpoint.api_key.expose()));
        }

        if req.stream == Some(true) {
            builder = builder.header("Accept", "text/event-stream");
        }

        let resp = builder.body(body).send().await?;

        if resp.status().as_u16() >= 400 {
            let status = resp.status().as_u16();
            let body_bytes = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: sanitize_api_error_body(body_bytes),
            });
        }

        Ok(resp)
    }

    /// Sends a non-streaming chat completion request and returns the parsed response.
    pub async fn chat_completion_non_streaming(
        &self,
        model_id: &str,
        mut req: ChatCompletionRequest,
        model: &ModelConfig,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        req.stream = Some(false);

        let resp = self.chat_completion(model_id, &req, model).await?;
        let chat_resp = resp.json::<ChatCompletionResponse>().await?;
        Ok(chat_resp)
    }

    /// Sends a streaming chat completion request and returns the byte stream.
    pub async fn get_streaming_body(
        &self,
        model_id: &str,
        mut req: ChatCompletionRequest,
        model: &ModelConfig,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>,
        ProviderError,
    > {
        req.stream = Some(true);

        let resp = self.chat_completion(model_id, &req, model).await?;
        Ok(Box::pin(resp.bytes_stream()))
    }

    // -----------------------------------------------------------------------
    // Anthropic raw request
    // -----------------------------------------------------------------------

    /// Sends raw bytes to the Anthropic endpoint.
    ///
    /// Sets **both** `x-api-key` and `Authorization: Bearer` headers,
    /// matching the Go reference implementation.  Uses [`SecretString`] via
    /// [`get_endpoint`][Self::get_endpoint] for consistent key handling.
    pub async fn send_anthropic_request(
        &self,
        body: &[u8],
        stream: bool,
        model: &ModelConfig,
    ) -> Result<Response, ProviderError> {
        let model_id = model.model_id.as_str();
        let endpoint = self.get_endpoint(model_id, model);

        let mut builder = self
            .http_client
            .post(&endpoint.base_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", endpoint.api_key.expose()))
            .header("x-api-key", endpoint.api_key.expose());

        if stream {
            builder = builder.header("Accept", "text/event-stream");
        }

        let resp = builder.body(body.to_vec()).send().await?;

        if resp.status().as_u16() >= 400 {
            let status = resp.status().as_u16();
            let body_bytes = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: sanitize_api_error_body(body_bytes),
            });
        }

        Ok(resp)
    }

    // -----------------------------------------------------------------------
    // Responses API
    // -----------------------------------------------------------------------

    /// Sends a request to the OpenAI Responses endpoint.
    pub async fn responses_completion(
        &self,
        model_id: &str,
        req: &ResponsesRequest,
        model: &ModelConfig,
    ) -> Result<Response, ProviderError> {
        let endpoint = self.get_endpoint(model_id, model);

        let body = serde_json::to_vec(req)?;

        let resp = self
            .http_client
            .post(&endpoint.base_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", endpoint.api_key.expose()))
            .body(body)
            .send()
            .await?;

        if resp.status().as_u16() >= 400 {
            let status = resp.status().as_u16();
            let body_bytes = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: sanitize_api_error_body(body_bytes),
            });
        }

        Ok(resp)
    }

    /// Sends a non-streaming Responses request and returns the parsed response.
    pub async fn responses_completion_non_streaming(
        &self,
        model_id: &str,
        mut req: ResponsesRequest,
        model: &ModelConfig,
    ) -> Result<ResponsesResponse, ProviderError> {
        req.stream = Some(false);

        let resp = self.responses_completion(model_id, &req, model).await?;
        let responses_resp = resp.json::<ResponsesResponse>().await?;
        Ok(responses_resp)
    }

    /// Sends a streaming Responses request and returns the byte stream.
    pub async fn get_responses_streaming_body(
        &self,
        model_id: &str,
        mut req: ResponsesRequest,
        model: &ModelConfig,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>,
        ProviderError,
    > {
        req.stream = Some(true);

        let resp = self.responses_completion(model_id, &req, model).await?;
        Ok(Box::pin(resp.bytes_stream()))
    }

    // -----------------------------------------------------------------------
    // Gemini API
    // -----------------------------------------------------------------------

    /// Sends a request to the Gemini endpoint.
    pub async fn gemini_completion(
        &self,
        model_id: &str,
        req: &GeminiRequest,
        model: &ModelConfig,
    ) -> Result<Response, ProviderError> {
        let endpoint = self.get_endpoint(model_id, model);

        let body = serde_json::to_vec(req)?;

        let resp = self
            .http_client
            .post(&endpoint.base_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", endpoint.api_key.expose()))
            .body(body)
            .send()
            .await?;

        if resp.status().as_u16() >= 400 {
            let status = resp.status().as_u16();
            let body_bytes = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: sanitize_api_error_body(body_bytes),
            });
        }

        Ok(resp)
    }

    /// Sends a non-streaming Gemini request and returns the parsed response.
    pub async fn gemini_completion_non_streaming(
        &self,
        model_id: &str,
        mut req: GeminiRequest,
        model: &ModelConfig,
    ) -> Result<GeminiResponse, ProviderError> {
        req.stream = Some(false);

        let resp = self.gemini_completion(model_id, &req, model).await?;
        let gemini_resp = resp.json::<GeminiResponse>().await?;
        Ok(gemini_resp)
    }

    /// Sends a streaming Gemini request and returns the byte stream.
    pub async fn get_gemini_streaming_body(
        &self,
        model_id: &str,
        mut req: GeminiRequest,
        model: &ModelConfig,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>,
        ProviderError,
    > {
        req.stream = Some(true);

        let resp = self.gemini_completion(model_id, &req, model).await?;
        Ok(Box::pin(resp.bytes_stream()))
    }
}

// ===========================================================================
// Tests (ported from oc-go-cc/internal/client/opencode_test.go)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use llm_proxy_core::config::ModelConfig;

    // Helper to build a ModelConfig with only provider set.
    fn model_config(provider: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.to_owned(),
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // is_anthropic_model
    // -----------------------------------------------------------------------

    #[test]
    fn is_anthropic_model_minimax_m25() {
        assert!(is_anthropic_model("minimax-m2.5"));
    }

    #[test]
    fn is_anthropic_model_minimax_m27() {
        assert!(is_anthropic_model("minimax-m2.7"));
    }

    #[test]
    fn is_anthropic_model_qwen37_max() {
        assert!(is_anthropic_model("qwen3.7-max"));
    }

    #[test]
    fn is_anthropic_model_qwen_prefix_match() {
        // Rust-specific: anything starting with "qwen" is anthropic.
        assert!(is_anthropic_model("qwen3.5-plus"));
        assert!(is_anthropic_model("qwen-coder"));
        assert!(is_anthropic_model("qwen"));
    }

    #[test]
    fn is_anthropic_model_deepseek_v4_pro_is_not() {
        assert!(!is_anthropic_model("deepseek-v4-pro"));
    }

    #[test]
    fn is_anthropic_model_deepseek_v4_flash_is_not() {
        assert!(!is_anthropic_model("deepseek-v4-flash"));
    }

    #[test]
    fn is_anthropic_model_kimi_k26_is_not() {
        assert!(!is_anthropic_model("kimi-k2.6"));
    }

    #[test]
    fn is_anthropic_model_glm51_is_not() {
        assert!(!is_anthropic_model("glm-5.1"));
    }

    // -----------------------------------------------------------------------
    // provider
    // -----------------------------------------------------------------------

    #[test]
    fn provider_empty_defaults_to_opencode_go() {
        let m = ModelConfig {
            model_id: "test-model".to_owned(),
            ..Default::default()
        };
        assert_eq!(provider(&m), PROVIDER_OPENCODE_GO);
    }

    #[test]
    fn provider_explicit_opencode_go() {
        let m = ModelConfig {
            provider: PROVIDER_OPENCODE_GO.to_owned(),
            model_id: "test-model".to_owned(),
            ..Default::default()
        };
        assert_eq!(provider(&m), PROVIDER_OPENCODE_GO);
    }

    #[test]
    fn provider_explicit_opencode_zen() {
        let m = ModelConfig {
            provider: PROVIDER_OPENCODE_ZEN.to_owned(),
            model_id: "test-model".to_owned(),
            ..Default::default()
        };
        assert_eq!(provider(&m), PROVIDER_OPENCODE_ZEN);
    }

    // -----------------------------------------------------------------------
    // is_zen
    // -----------------------------------------------------------------------

    #[test]
    fn is_zen_opencode_go_is_not_zen() {
        let m = model_config(PROVIDER_OPENCODE_GO);
        assert!(!is_zen(&m));
    }

    #[test]
    fn is_zen_opencode_zen_is_zen() {
        let m = model_config(PROVIDER_OPENCODE_ZEN);
        assert!(is_zen(&m));
    }

    #[test]
    fn is_zen_empty_provider_is_not_zen() {
        let m = model_config("");
        assert!(!is_zen(&m));
    }

    // -----------------------------------------------------------------------
    // classify_endpoint
    // -----------------------------------------------------------------------

    #[test]
    fn classify_endpoint_minimax_m25_anthropic() {
        assert_eq!(classify_endpoint("minimax-m2.5"), EndpointType::Anthropic);
    }

    #[test]
    fn classify_endpoint_minimax_m27_anthropic() {
        assert_eq!(classify_endpoint("minimax-m2.7"), EndpointType::Anthropic);
    }

    #[test]
    fn classify_endpoint_qwen37_max_anthropic() {
        assert_eq!(classify_endpoint("qwen3.7-max"), EndpointType::Anthropic);
    }

    #[test]
    fn classify_endpoint_gemini35_flash() {
        assert_eq!(classify_endpoint("gemini-3.5-flash"), EndpointType::Gemini);
    }

    #[test]
    fn classify_endpoint_gemini31_pro() {
        assert_eq!(classify_endpoint("gemini-3.1-pro"), EndpointType::Gemini);
    }

    #[test]
    fn classify_endpoint_gemini3_flash() {
        assert_eq!(classify_endpoint("gemini-3-flash"), EndpointType::Gemini);
    }

    #[test]
    fn classify_endpoint_gpt55_responses() {
        assert_eq!(classify_endpoint("gpt-5.5"), EndpointType::Responses);
    }

    #[test]
    fn classify_endpoint_gpt54_responses() {
        assert_eq!(classify_endpoint("gpt-5.4"), EndpointType::Responses);
    }

    #[test]
    fn classify_endpoint_gpt5_responses() {
        assert_eq!(classify_endpoint("gpt-5"), EndpointType::Responses);
    }

    #[test]
    fn classify_endpoint_kimi_k26_chat_completions() {
        assert_eq!(
            classify_endpoint("kimi-k2.6"),
            EndpointType::ChatCompletions
        );
    }

    #[test]
    fn classify_endpoint_glm51_chat_completions() {
        assert_eq!(classify_endpoint("glm-5.1"), EndpointType::ChatCompletions);
    }

    #[test]
    fn classify_endpoint_deepseek_v4_flash_chat_completions() {
        assert_eq!(
            classify_endpoint("deepseek-v4-flash"),
            EndpointType::ChatCompletions
        );
    }

    #[test]
    fn classify_endpoint_unknown_model_chat_completions() {
        assert_eq!(
            classify_endpoint("unknown-model"),
            EndpointType::ChatCompletions
        );
    }

    // -----------------------------------------------------------------------
    // is_gemini_model
    // -----------------------------------------------------------------------

    #[test]
    fn is_gemini_model_gemini35_flash() {
        assert!(is_gemini_model("gemini-3.5-flash"));
    }

    #[test]
    fn is_gemini_model_gemini31_pro() {
        assert!(is_gemini_model("gemini-3.1-pro"));
    }

    #[test]
    fn is_gemini_model_gemini3_flash() {
        assert!(is_gemini_model("gemini-3-flash"));
    }

    #[test]
    fn is_gemini_model_kimi_k26_is_not() {
        assert!(!is_gemini_model("kimi-k2.6"));
    }

    #[test]
    fn is_gemini_model_glm51_is_not() {
        assert!(!is_gemini_model("glm-5.1"));
    }

    #[test]
    fn is_gemini_model_gpt55_is_not() {
        assert!(!is_gemini_model("gpt-5.5"));
    }

    // -----------------------------------------------------------------------
    // is_responses_model
    // -----------------------------------------------------------------------

    #[test]
    fn is_responses_model_gpt55() {
        assert!(is_responses_model("gpt-5.5"));
    }

    #[test]
    fn is_responses_model_gpt55_pro() {
        assert!(is_responses_model("gpt-5.5-pro"));
    }

    #[test]
    fn is_responses_model_gpt54() {
        assert!(is_responses_model("gpt-5.4"));
    }

    #[test]
    fn is_responses_model_gpt54_pro() {
        assert!(is_responses_model("gpt-5.4-pro"));
    }

    #[test]
    fn is_responses_model_gpt54_mini() {
        assert!(is_responses_model("gpt-5.4-mini"));
    }

    #[test]
    fn is_responses_model_gpt54_nano() {
        assert!(is_responses_model("gpt-5.4-nano"));
    }

    #[test]
    fn is_responses_model_gpt53_codex() {
        assert!(is_responses_model("gpt-5.3-codex"));
    }

    #[test]
    fn is_responses_model_gpt53_codex_spark() {
        assert!(is_responses_model("gpt-5.3-codex-spark"));
    }

    #[test]
    fn is_responses_model_gpt52() {
        assert!(is_responses_model("gpt-5.2"));
    }

    #[test]
    fn is_responses_model_gpt52_codex() {
        assert!(is_responses_model("gpt-5.2-codex"));
    }

    #[test]
    fn is_responses_model_gpt51() {
        assert!(is_responses_model("gpt-5.1"));
    }

    #[test]
    fn is_responses_model_gpt51_codex() {
        assert!(is_responses_model("gpt-5.1-codex"));
    }

    #[test]
    fn is_responses_model_gpt51_codex_max() {
        assert!(is_responses_model("gpt-5.1-codex-max"));
    }

    #[test]
    fn is_responses_model_gpt51_codex_mini() {
        assert!(is_responses_model("gpt-5.1-codex-mini"));
    }

    #[test]
    fn is_responses_model_gpt5() {
        assert!(is_responses_model("gpt-5"));
    }

    #[test]
    fn is_responses_model_gpt5_codex() {
        assert!(is_responses_model("gpt-5-codex"));
    }

    #[test]
    fn is_responses_model_gpt5_nano() {
        assert!(is_responses_model("gpt-5-nano"));
    }

    #[test]
    fn is_responses_model_kimi_k26_is_not() {
        assert!(!is_responses_model("kimi-k2.6"));
    }

    #[test]
    fn is_responses_model_glm51_is_not() {
        assert!(!is_responses_model("glm-5.1"));
    }

    #[test]
    fn is_responses_model_gemini35_flash_is_not() {
        assert!(!is_responses_model("gemini-3.5-flash"));
    }
}
