//! Provider error types shared across transport, adapters, and routes.
//!
//! Extracted from `client.rs` so that the new transport layer (Phase 4) and
//! future provider adapters can depend on [`ProviderError`] without coupling to
//! the legacy `OpenCodeClient`.

// ---------------------------------------------------------------------------
// Provider error
// ---------------------------------------------------------------------------

/// Errors produced by provider transport and adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Failed to serialize or deserialize JSON.
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

    /// An SSE framing error occurred while parsing the upstream stream.
    #[error("SSE framing error: {0}")]
    Sse(String),

    /// Invalid UTF-8 encountered in streamed bytes.
    #[error("invalid UTF-8 in stream: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Maximum length for upstream API error bodies stored in [`ProviderError::Api`].
pub(crate) const MAX_API_ERROR_BODY_LEN: usize = 512;

/// Sanitize an upstream API error body: strip common key patterns and
/// truncate to [`MAX_API_ERROR_BODY_LEN`].
///
/// Covers:
/// - OpenAI keys: `sk-live-...`, `sk-test-...`, `sk-...`
/// - Anthropic keys: `sk-ant-api03-...`, `sk-ant-...`
/// - Google API keys: `AIza...`
/// - Generic key prefixes: `key-...`
pub fn sanitize_api_error_body(mut body: String) -> String {
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
