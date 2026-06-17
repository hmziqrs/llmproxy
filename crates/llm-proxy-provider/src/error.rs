//! Provider error types shared across transport, adapters, and routes.
//!
//! Extracted so that the transport layer and provider adapters can depend on
//! [`ProviderError`] without coupling to any specific client implementation.

// ---------------------------------------------------------------------------
// Provider error
// ---------------------------------------------------------------------------

/// Errors produced by provider transport and adapter operations.
///
/// This enum is non-exhaustive; downstream code should include a catch-all arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// Failed to serialize or deserialize JSON.
    #[error("failed to marshal request: {0}")]
    Serialize(#[from] serde_json::Error),

    /// The HTTP request failed at the transport level.
    ///
    /// The message is stored without the request URL so query credentials
    /// cannot leak through logs, CLI output, or downstream responses.
    #[error("request failed: {message}")]
    Http {
        /// URL-free transport error text.
        message: String,
        /// Whether reqwest classified the failure as a timeout.
        timeout: bool,
    },

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

impl From<reqwest::Error> for ProviderError {
    fn from(error: reqwest::Error) -> Self {
        let timeout = error.is_timeout();
        Self::Http {
            message: error.without_url().to_string(),
            timeout,
        }
    }
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
/// Delegates to the shared implementation in `llm_proxy_protocol::util`.
fn truncate_with_suffix(s: &str, max_len: usize, suffix: &str) -> String {
    llm_proxy_protocol::util::truncate_with_suffix(s, max_len, suffix)
}

/// Redaction patterns compiled once via `LazyLock`.
///
/// Each pattern matches a known API-key prefix followed by enough token
/// characters to be a real key (10–20+). This avoids false positives on short
/// substrings like `sk-` that appear in ordinary words (e.g. "desk-area").
static REDACTION_PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        // Character class covering the URL/JSON-safe punctuation real tokens
        // carry: `.` (dotted OpenAI restricted keys, JWT segment separators),
        // `-`, `_`, `/`, `+`, `=` (base64). Delimiters — quotes, braces,
        // brackets, commas, whitespace — are deliberately excluded so a
        // trailing delimiter adjacent to the token is never consumed into the
        // redaction. Prior to GAP-MED-1 this class stopped at `.`/`/`/`+`/`=`,
        // so a dotted key or JWT was only redacted up to its first dot and the
        // secret-bearing suffix leaked to the client.
        const C: &str = r"A-Za-z0-9_.\-/+=";
        // Order matters only for intent-documentation: every pattern runs a
        // full `replace_all` pass over the body, and `***` is inert, so the
        // final output is the union of all matches regardless of order.
        // Longer/more-specific patterns are listed first.
        [
            // OpenAI restricted (dotted) keys: sk-proj-XXXX.YYYY
            format!(r"sk-proj-[{C}]{{10,}}"),
            // Anthropic keys: sk-ant-api03-XXXXX
            format!(r"sk-ant-api03-[{C}]{{10,}}"),
            // Anthropic keys: sk-ant-XXXXX
            format!(r"sk-ant-[{C}]{{10,}}"),
            // OpenAI keys: sk-live-XXXXX (hyphen form)
            format!(r"sk-live-[{C}]{{10,}}"),
            // OpenAI keys: sk-test-XXXXX (hyphen form)
            format!(r"sk-test-[{C}]{{10,}}"),
            // OpenAI keys: sk_live_XXXXX (underscore form)
            format!(r"sk_live_[{C}]{{10,}}"),
            // OpenAI keys: sk_test_XXXXX (underscore form)
            format!(r"sk_test_[{C}]{{10,}}"),
            // Generic sk- prefix with enough trailing chars to look like a key
            format!(r"sk-[{C}]{{20,}}"),
            // Google API keys: AIza followed by 30+ token chars
            format!(r"AIza[{C}]{{30,}}"),
            // Generic key- prefix with enough trailing chars
            format!(r"key-[{C}]{{20,}}"),
            // JWT-shaped Bearer tokens: Bearer eyJ....body.sig (lowered bar so
            // even a partially-echoed JWT fragment is caught).
            format!(r"Bearer eyJ[{C}]{{10,}}"),
            // Bearer token values echoed in error responses
            format!(r"Bearer [{C}]{{20,}}"),
            // Generic key= assignment patterns (key=VALUE with 20+ chars)
            format!(r"(?i)key=[{C}]{{20,}}"),
            // Generic token= assignment patterns (token=VALUE with 20+ chars)
            format!(r"(?i)token=[{C}]{{20,}}"),
        ]
        .iter()
        // SAFETY: every pattern is built from a static string literal (via
        // format! with a compile-time constant char class), so the resulting
        // regex is known valid and `Regex::new` cannot fail here.
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
///   `sk-proj-...` (dotted restricted keys), generic `sk-...` (only when
///   followed by 20+ token chars).
/// - Anthropic keys: `sk-ant-api03-...`, `sk-ant-...`
/// - Google API keys: `AIza...` (only when followed by 30+ chars)
/// - Generic key prefixes: `key-...` (only when followed by 20+ chars)
/// - Generic assignment patterns: `key=...`, `token=...` (case-insensitive,
///   only when followed by 20+ chars)
/// - Bearer token values: `Bearer ...` (only when followed by 20+ chars) and
///   JWT-shaped `Bearer eyJ....body.sig`
///
/// The token character class includes `.`/`/`/`+`/`=` so dotted keys and JWTs
/// are redacted whole rather than only up to their first `.` (audit GAP-MED-1).
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
    // Both implementations must stay in sync.
    if body.len() > MAX_API_ERROR_BODY_LEN {
        body = truncate_with_suffix(&body, MAX_API_ERROR_BODY_LEN, TRUNCATED_SUFFIX);
    }
    body
}

// ---------------------------------------------------------------------------
// ProviderError convenience constructors
// ---------------------------------------------------------------------------

impl ProviderError {
    /// Return whether this is an HTTP timeout.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Http { timeout: true, .. })
    }

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
                // All remaining status codes (including 4xx not listed above)
                // are classified as Upstream errors. This is intentional since
                // ProviderError::Api is only constructed for status >= 400.
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

    #[test]
    fn redaction_patterns_all_compile_successfully() {
        // Verify every regex in REDACTION_PATTERNS compiles without panicking.
        // This is a regression guard: if a pattern is edited to be invalid,
        // the LazyLock would panic at first use in production. This test
        // exercises the initialization path explicitly.
        let _ = &*REDACTION_PATTERNS;
    }

    // -----------------------------------------------------------------------
    // GAP-MED-1: dotted keys / JWTs must redact whole (no suffix leak)
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_redacts_dotted_openai_proj_key_with_no_suffix_leak() {
        // OpenAI restricted key echoed in a dotted format. The pre-GAP-MED-1
        // char class stopped at `.` so only `sk-proj-AbCd...` matched and the
        // `.T3BlbkFJ...` suffix leaked. The whole token must now redact.
        let body = "invalid key: sk-proj-AbCdEfGh1234567890.T3BlbkFJabc123def456".to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("T3BlbkFJ") && !result.contains("AbCdEfGh"),
            "dotted key suffix must not leak: {result}"
        );
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_redacts_jwt_bearer_token_with_no_suffix_leak() {
        // A `Bearer <JWT>` is three dot-separated base64url segments. The
        // pre-GAP-MED-1 class stopped at `.` so only the header segment
        // matched and the payload + signature leaked.
        let jwt = concat!(
            "Bearer eyJhbGciOiJIUzI1NiJ9.",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0.",
            "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        );
        let result = sanitize_api_error_body(jwt.to_owned());
        assert_eq!(
            result, "***",
            "the entire JWT (payload + signature) must redact, got: {result}"
        );
    }

    #[test]
    fn sanitize_redacts_legacy_dotted_key_with_no_suffix_leak() {
        // A legacy dotted key (no recognized prefix) is still caught by the
        // broadened generic `key=` / `sk-` patterns as long as it carries a
        // known prefix; here the `key=` assignment form redacts the dotted
        // value whole.
        let body = "error: key=abCdEf.1234567890abcdefghij.KLuMnO".to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("KLuMnO") && !result.contains("1234567890abcdefghij"),
            "dotted key= value must redact whole: {result}"
        );
        assert!(result.contains("***"));
    }

    #[test]
    fn sanitize_does_not_consume_trailing_delimiter_after_token() {
        // A token immediately followed by a quote/brace must redact the token
        // but leave the delimiter in place — the char class excludes `"` `}`
        // etc. so the suffix cannot be swallowed.
        let body = r#"{"error":"key sk-ant-abc123def456ghi789jkl012mno345pqr"}"#.to_owned();
        let result = sanitize_api_error_body(body);
        assert!(
            !result.contains("abc123def456ghi789"),
            "token body must be redacted: {result}"
        );
        assert!(
            result.contains("\"}"),
            "trailing quote/brace must be preserved, got: {result}"
        );
    }

    #[tokio::test]
    async fn http_error_does_not_expose_request_url_or_query_secret() {
        let secret = "query-secret-that-must-not-leak";
        let error = reqwest::Client::new()
            .get(format!("http://127.0.0.1:0/models?key={secret}"))
            .send()
            .await
            .expect_err("port zero must fail");
        let rendered = ProviderError::from(error).to_string();
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("127.0.0.1"));
        assert!(!rendered.contains("models?key="));
    }
}
