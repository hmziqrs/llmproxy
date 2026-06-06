//! Provider error types shared across transport, adapters, and routes.
//!
//! Extracted so that the transport layer and provider adapters can depend on
//! [`ProviderError`] without coupling to any specific client implementation.

// ---------------------------------------------------------------------------
// Provider error
// ---------------------------------------------------------------------------

/// Errors produced by provider transport and adapter operations.
///
/// This enum may grow new variants in future phases; match exhaustively at your
/// own risk. Prefer `match` with a catch-all `_ =>` arm in downstream code.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// Failed to serialize or deserialize JSON.
    #[error("failed to marshal request: {0}")]
    Serialize(#[from] serde_json::Error),

    /// The HTTP request failed at the transport level.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// The upstream API returned an error status code.
    ///
    /// The `body` field is truncated to the internal maximum error body length
    /// (512 bytes) and stripped of common API key patterns at construction time
    /// so that `Display` output (used in `warn!()` / `error!()` logging) never
    /// contains full key material.
    #[error("API error {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body text (truncated and sanitized).
        body: String,
    },

    /// Invalid UTF-8 encountered in streamed bytes.
    #[error("invalid UTF-8 in stream: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    /// SSE framing error (malformed frame structure, unexpected stream termination).
    ///
    /// Constructed by [`SseFramer`](crate::sse::SseFramer) when it detects
    /// malformed SSE structure such as unterminated frames after stream end.
    #[error("SSE framing error: {0}")]
    SseFraming(String),

    /// The upstream provider returned an empty response (no choices, no candidates).
    #[error("empty response from upstream: {0}")]
    EmptyResponse(String),

    /// Invalid configuration or input (e.g. unsafe model name for URL interpolation).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

/// Maximum length for upstream API error bodies stored in [`ProviderError::Api`].
///
/// After truncation the string is at most `MAX_API_ERROR_BODY_LEN` bytes long
/// (the `...[truncated]` suffix is included within this budget).
pub(crate) const MAX_API_ERROR_BODY_LEN: usize = 512;

/// Suffix appended when a body exceeds the truncation limit.
const TRUNCATED_SUFFIX: &str = "...[truncated]";

/// Truncate a string to `max_len` bytes, appending `suffix` if truncation occurs.
///
/// The final string is at most `max_len` bytes (the suffix is included within
/// this budget). Respects UTF-8 char boundaries to prevent panics on multi-byte
/// characters.
///
/// This is the same logic as `llm_proxy_server::routes::error_response::truncate_with_suffix`.
/// Both implementations must stay in sync. If a shared utility crate is
/// introduced in a future phase, both should call into it.
fn truncate_with_suffix(s: &str, max_len: usize, suffix: &str) -> String {
    if s.len() <= max_len {
        s.to_owned()
    } else {
        let max_content = max_len - suffix.len();
        let mut end = max_content;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}{}", &s[..end], suffix)
    }
}

/// Redaction patterns compiled once via `LazyLock`.
///
/// Each pattern matches a known API-key prefix followed by enough alphanumeric
/// characters to be a real key (20+). This avoids false positives on short
/// substrings like `sk-` that appear in ordinary words (e.g. "desk-area").
static REDACTION_PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        // Order matters: longer/more-specific patterns first.
        [
            // Anthropic keys: sk-ant-api03-XXXXX
            r"sk-ant-api03-[A-Za-z0-9_-]{10,}",
            // Anthropic keys: sk-ant-XXXXX
            r"sk-ant-[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk-live-XXXXX (hyphen form)
            r"sk-live-[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk-test-XXXXX (hyphen form)
            r"sk-test-[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk_live_XXXXX (underscore form)
            r"sk_live_[A-Za-z0-9_-]{10,}",
            // OpenAI keys: sk_test_XXXXX (underscore form)
            r"sk_test_[A-Za-z0-9_-]{10,}",
            // Generic sk- prefix with enough trailing chars to look like a key
            r"sk-[A-Za-z0-9_-]{20,}",
            // Google API keys: AIza followed by 30+ alphanumeric chars
            r"AIza[A-Za-z0-9_-]{30,}",
            // Generic key- prefix with enough trailing chars
            r"key-[A-Za-z0-9_-]{20,}",
            // Bearer token values echoed in error responses
            r"Bearer [A-Za-z0-9_-]{20,}",
            // Generic key= assignment patterns (key=VALUE with 20+ chars)
            r"(?i)key=[A-Za-z0-9_-]{20,}",
            // Generic token= assignment patterns (token=VALUE with 20+ chars)
            r"(?i)token=[A-Za-z0-9_-]{20,}",
        ]
        .iter()
        // SAFETY: all patterns are static string literals known at compile time,
        // so regex::Regex::new cannot fail here.
        .map(|pat| regex::Regex::new(pat).expect("invalid redaction regex"))
        .collect()
    });

/// Sanitize an upstream API error body: strip common key patterns and
/// truncate to [`MAX_API_ERROR_BODY_LEN`].
///
/// # Ordering rationale
///
/// Redaction runs BEFORE truncation. This means if a key pattern appears in
/// the portion that would be truncated away, the pattern survives because
/// truncation happens after redaction. This is acceptable because:
/// 1. The truncation limit (512 bytes) is large enough to cover the vast
///    majority of real error messages.
/// 2. The redaction patterns target key *prefixes* (e.g. `sk-ant-`) which
///    typically appear near the start of error bodies, not near the end.
/// 3. Even if a partial key survives in the truncated tail, the key is
///    incomplete and unlikely to be the full secret.
///
/// # Covered patterns
///
/// - OpenAI keys: `sk-live-...`, `sk-test-...`, `sk_live_...`, `sk_test_...`,
///   generic `sk-...` (only when followed by 20+ alphanumeric chars).
/// - Anthropic keys: `sk-ant-api03-...`, `sk-ant-...`
/// - Google API keys: `AIza...` (only when followed by 30+ chars)
/// - Generic key prefixes: `key-...` (only when followed by 20+ chars)
/// - Generic assignment patterns: `key=...`, `token=...` (case-insensitive,
///   only when followed by 20+ chars)
/// - Bearer token values: `Bearer ...` (only when followed by 20+ chars)
///
/// Uses regex-based matching to avoid false-positive redaction of short
/// substrings like `sk-` or `key-` that appear in ordinary words.
pub(crate) fn sanitize_api_error_body(mut body: String) -> String {
    // Redact API key patterns (regex-based, avoids false positives on short substrings).
    for re in REDACTION_PATTERNS.iter() {
        body = re.replace_all(&body, "***").into_owned();
    }

    // Truncate if the sanitized body exceeds the limit.
    // Uses the same truncate-with-suffix logic as
    // `llm_proxy_server::routes::error_response::truncate_with_suffix`.
    // Both implementations must stay in sync. If a shared utility crate is
    // introduced in a future phase, both should call into it.
    if body.len() > MAX_API_ERROR_BODY_LEN {
        body = truncate_with_suffix(&body, MAX_API_ERROR_BODY_LEN, TRUNCATED_SUFFIX);
    }
    body
}

// ---------------------------------------------------------------------------
// ProviderError convenience constructors
// ---------------------------------------------------------------------------

impl ProviderError {
    /// Construct a [`ProviderError::Api`] with automatic body sanitization.
    ///
    /// Encapsulates the sanitization call so callers never need to remember
    /// to call the internal `sanitize_api_error_body` function manually.
    pub fn api(status: u16, body_text: String) -> Self {
        Self::Api {
            status,
            body: sanitize_api_error_body(body_text),
        }
    }

    /// Classify the HTTP status code of a [`ProviderError::Api`] into a
    /// [`llm_proxy_protocol::core::CoreStreamErrorKind`].
    ///
    /// Returns `None` for non-`Api` variants.
    pub fn api_error_kind(&self) -> Option<llm_proxy_protocol::core::CoreStreamErrorKind> {
        match self {
            Self::Api { status, .. } => Some(match *status {
                400 => llm_proxy_protocol::core::CoreStreamErrorKind::InvalidRequest,
                401 => llm_proxy_protocol::core::CoreStreamErrorKind::Authentication,
                403 => llm_proxy_protocol::core::CoreStreamErrorKind::Permission,
                429 => llm_proxy_protocol::core::CoreStreamErrorKind::RateLimit,
                500 | 502 | 503 => llm_proxy_protocol::core::CoreStreamErrorKind::Upstream,
                _ => llm_proxy_protocol::core::CoreStreamErrorKind::Upstream,
            }),
            _ => None,
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // sanitize_api_error_body tests
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_body_under_limit_is_unchanged() {
        let body = "simple error message".to_owned();
        assert_eq!(sanitize_api_error_body(body.clone()), body);
    }

    #[test]
    fn sanitize_body_over_limit_is_truncated() {
        let body = "x".repeat(600);
        let result = sanitize_api_error_body(body);
        assert!(result.ends_with("...[truncated]"));
        assert!(result.len() <= MAX_API_ERROR_BODY_LEN);
    }

    #[test]
    fn sanitize_body_truncation_exact_length() {
        // Exactly at the limit should NOT be truncated.
        let body = "a".repeat(MAX_API_ERROR_BODY_LEN);
        let result = sanitize_api_error_body(body.clone());
        assert_eq!(result, body);

        // One over the limit should be truncated.
        let body = "a".repeat(MAX_API_ERROR_BODY_LEN + 1);
        let result = sanitize_api_error_body(body);
        assert!(result.ends_with("...[truncated]"));
        assert!(result.len() <= MAX_API_ERROR_BODY_LEN);
    }

    #[test]
    fn sanitize_body_multibyte_char_at_boundary_does_not_panic() {
        // Japanese characters are 3 bytes each in UTF-8.
        // Build a body whose byte length straddles the truncation boundary.
        let body = "あ".repeat(200); // 600 bytes, exceeds 512
        let result = sanitize_api_error_body(body);
        // Must not panic and must be valid UTF-8.
        assert!(result.len() <= MAX_API_ERROR_BODY_LEN);
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn sanitize_redacts_openai_sk_live_underscore() {
        let body = r#"error: key=sk_live_abc123def456ghi789jkl012mno"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("sk_live_"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_openai_sk_test_underscore() {
        let body = r#"error: key=sk_test_abc123def456ghi789jkl012mno"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("sk_test_"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_openai_sk_live_hyphen() {
        let body = r#"error: key=sk-live-abc123def456ghi789jkl012mno"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("sk-live-"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_openai_sk_test_hyphen() {
        let body = r#"error: key=sk-test-abc123def456ghi789jkl012mno"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("sk-test-"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_anthropic_sk_ant_api03() {
        let body = r#"error: key=sk-ant-api03-abc123def456ghi789jkl012"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("sk-ant-api03-"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_anthropic_sk_ant() {
        let body = r#"error: key=sk-ant-abc123def456ghi789jkl012mno345"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("sk-ant-"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_google_aiza() {
        let body = r#"error: key=AIzaSyD-abc123def456ghi789jkl012mno345pqr"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("AIzaSyD-abc"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_generic_key_prefix() {
        let body = r#"error: key=key-abc123def456ghi789jkl012mno345pqr"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(!result.contains("key-abc123def456ghi789"));
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_does_not_redact_short_sk_substring() {
        // "desk-area" should NOT be redacted -- `sk-` only matches with 20+ trailing chars.
        let body = "the desk-area is reserved".to_owned();
        let result = sanitize_api_error_body(body.clone());
        assert_eq!(result, body);
    }

    #[test]
    fn sanitize_does_not_redact_short_key_substring() {
        // "key-value store" should NOT be redacted.
        let body = "key-value store".to_owned();
        let result = sanitize_api_error_body(body.clone());
        assert_eq!(result, body);
    }

    #[test]
    fn sanitize_does_not_redact_short_aiza_substring() {
        // "AIza" alone without 30+ trailing chars should not be redacted.
        let body = "The area is at AIza Lane".to_owned();
        let result = sanitize_api_error_body(body.clone());
        assert_eq!(result, body);
    }

    #[test]
    fn sanitize_redacts_bearer_token_in_error_body() {
        // If an upstream API echoes the Authorization header value back in its
        // error response, the Bearer prefix + token must be redacted.
        let body = r#"error: invalid key Bearer sk-ant-api03-abc123def456ghi789jkl012"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("sk-ant-api03-abc123def456ghi789"),
            "Bearer token must be redacted: {}",
            result
        );
        assert!(
            result.contains("***"),
            "Must contain redaction marker: {}",
            result
        );
    }

    #[test]
    fn sanitize_redacts_generic_key_assignment() {
        let body = r#"error: key=sk_live_abc123def456ghi789jkl012mno345"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("sk_live_abc123def456ghi789"),
            "key= assignment must be redacted: {}",
            result
        );
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_generic_token_assignment() {
        let body = r#"error: token=abc123def456ghi789jkl012mno345pqr"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("abc123def456ghi789jkl012"),
            "token= assignment must be redacted: {}",
            result
        );
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_key_assignment_case_insensitive() {
        let body = r#"error: KEY=abc123def456ghi789jkl012mno345pqr"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("abc123def456ghi789jkl012"),
            "KEY= (uppercase) must be redacted: {}",
            result
        );
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_does_not_redact_short_key_assignment() {
        // "key=short" should NOT be redacted -- under 20 chars.
        let body = "error: key=shortvalue".to_owned();
        let result = sanitize_api_error_body(body.clone());
        assert_eq!(result, body);
    }
}
