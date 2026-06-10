//! Middleware components for the LLM proxy server.
//!
//! Provides request deduplication, rate limiting, request ID generation,
//! and client IP extraction.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// RequestDeduplicator
// ---------------------------------------------------------------------------

/// Deduplicates requests based on a SHA-256 hash of the request path and body.
///
/// Tracks in-flight request hashes with a configurable deduplication window.
/// The standalone type defaults to 500 ms; server configuration defaults to
/// disabled. If a second request with the same path and body arrives during a
/// nonzero window, the duplicate is rejected.
///
/// The request path is included in the hash so that the same body sent to
/// different protocol endpoints (e.g. `/v1/messages` vs `/v1/chat/completions`)
/// is not treated as a duplicate.
///
/// # Why `std::sync::Mutex`?
///
/// Uses `std::sync::Mutex` rather than `tokio::sync::Mutex` because the
/// critical section is purely in-memory HashMap operations (insert, remove,
/// retain) that complete in microseconds. `tokio::sync::Mutex` would add
/// unnecessary async overhead for a non-I/O-bound lock. The mutex is never
/// held across `.await` points.
#[derive(Debug)]
pub struct RequestDeduplicator {
    /// SHA-256 hex digest -> insertion time.
    in_flight: Mutex<HashMap<String, Instant>>,
    /// Deduplication window in milliseconds.
    window_ms: u64,
}

impl RequestDeduplicator {
    /// Create a new deduplicator with a 500 ms window.
    pub fn new() -> Self {
        Self::with_window_ms(500)
    }

    /// Create a new deduplicator with a custom deduplication window.
    pub fn with_window_ms(window_ms: u64) -> Self {
        Self {
            in_flight: Mutex::new(HashMap::new()),
            window_ms,
        }
    }

    /// Check whether this request body is a duplicate.
    ///
    /// Returns `true` if the request is a duplicate (should be rejected),
    /// `false` if it is new (should be processed).
    pub fn is_duplicate(&self, body: &[u8]) -> bool {
        self.is_duplicate_with_path("", body)
    }

    /// Check whether this request is a duplicate, keyed by path and body.
    ///
    /// Including the path distinguishes requests to different protocol
    /// endpoints (e.g. `/v1/messages` vs `/v1/chat/completions`) that
    /// happen to carry the same body.
    pub fn is_duplicate_with_path(&self, path: &str, body: &[u8]) -> bool {
        if self.window_ms == 0 {
            return false;
        }
        let hash = Self::hash_path_body(path, body);
        let now = Instant::now();

        let mut map = self.in_flight.lock().unwrap_or_else(|e| {
            tracing::warn!("request deduplicator mutex poisoned; recovering from panic");
            e.into_inner()
        });

        // Prune expired entries.
        map.retain(|_, t| now.duration_since(*t).as_millis() < self.window_ms as u128);

        if map.contains_key(&hash) {
            return true;
        }

        map.insert(hash, now);
        false
    }

    /// Compute the SHA-256 hex digest of the request path and body.
    fn hash_path_body(path: &str, body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        hasher.update(body);
        format!("{:x}", hasher.finalize())
    }
}

impl Default for RequestDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RateLimiter
// ---------------------------------------------------------------------------

/// Per-client token bucket for rate limiting.
#[derive(Debug)]
struct ClientTokenBucket {
    /// Remaining tokens.
    tokens: f64,
    /// Maximum tokens.
    max_tokens: f64,
    /// Timestamp of last refill.
    last_refill: Instant,
}

impl ClientTokenBucket {
    fn new(max_tokens: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `true` if allowed.
    fn try_consume(&mut self, refill_rate: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * refill_rate).min(self.max_tokens);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Per-IP rate limiter using token buckets.
///
/// Each client IP gets its own bucket allowing `max_requests_per_minute`
/// requests per minute.
#[derive(Debug)]
pub struct RateLimiter {
    /// Per-IP token buckets.
    buckets: Mutex<HashMap<String, ClientTokenBucket>>,
    /// Maximum requests per minute per client.
    max_requests_per_minute: f64,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// `max_requests_per_minute` is the maximum number of requests allowed
    /// per client IP per minute. `0` disables rate limiting.
    pub fn new(max_requests_per_minute: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_requests_per_minute: max_requests_per_minute as f64,
        }
    }

    /// Check whether a request from `client_ip` is allowed.
    ///
    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn is_allowed(&self, client_ip: &str) -> bool {
        if self.max_requests_per_minute == 0.0 {
            return true;
        }
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| {
            tracing::warn!("rate limiter mutex poisoned; recovering from panic");
            e.into_inner()
        });

        // Evict stale entries to prevent unbounded memory growth.
        // Prune entries not accessed within the last 5 minutes. Always prune
        // (not just when above a threshold) so stale entries from low but
        // steady traffic do not accumulate indefinitely.
        let now = std::time::Instant::now();
        buckets.retain(|_, bucket| {
            now.duration_since(bucket.last_refill) < std::time::Duration::from_secs(300)
        });

        // Refill rate: tokens per second.
        let refill_rate = self.max_requests_per_minute / 60.0;

        let bucket = buckets
            .entry(client_ip.to_owned())
            .or_insert_with(|| ClientTokenBucket::new(self.max_requests_per_minute));

        bucket.try_consume(refill_rate)
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(100)
    }
}

// ---------------------------------------------------------------------------
// RequestIdGenerator
// ---------------------------------------------------------------------------

/// Generates unique request IDs in the format `req-{unix}-{counter}`.
#[derive(Debug)]
pub struct RequestIdGenerator {
    counter: AtomicU64,
}

impl RequestIdGenerator {
    /// Create a new request ID generator.
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    /// Generate the next request ID.
    pub fn next_id(&self) -> String {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("req-{unix}-{counter}")
    }
}

impl Default for RequestIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Client IP extraction
// ---------------------------------------------------------------------------

/// Optional peer socket metadata for handlers that also run in router-only tests.
#[derive(Clone, Copy, Debug)]
pub struct OptionalConnectInfo(pub Option<SocketAddr>);

impl<S> FromRequestParts<S> for OptionalConnectInfo
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let address = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|connect_info| connect_info.0);
        async move { Ok(Self(address)) }
    }
}

/// Extract the client IP from trusted forwarding headers or connection info.
pub fn get_client_ip(
    headers: &axum::http::HeaderMap,
    connect_info: Option<&SocketAddr>,
    trust_forwarded_headers: bool,
) -> String {
    if trust_forwarded_headers {
        // Check X-Forwarded-For first (leftmost IP).
        if let Some(xff) = headers.get("x-forwarded-for") {
            if let Ok(val) = xff.to_str() {
                if let Some(ip) = val.split(',').next() {
                    let trimmed = ip.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_owned();
                    }
                }
            }
        }

        // Check X-Real-Ip.
        if let Some(xri) = headers.get("x-real-ip") {
            if let Ok(val) = xri.to_str() {
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
            }
        }
    }

    // Fall back to connection info.
    if let Some(ci) = connect_info {
        return ci.ip().to_string();
    }

    "unknown".to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- RequestDeduplicator ---------------------------------------------------

    #[test]
    fn dedup_new_request_is_not_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate(b"hello"));
    }

    #[test]
    fn dedup_same_body_is_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate(b"hello"));
        assert!(dedup.is_duplicate(b"hello"));
    }

    #[test]
    fn dedup_different_body_is_not_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate(b"hello"));
        assert!(!dedup.is_duplicate(b"world"));
    }

    #[test]
    fn request_id_format() {
        let generator = RequestIdGenerator::new();
        let id = generator.next_id();
        assert!(id.starts_with("req-"));
        assert!(id.contains('-'));
    }

    #[test]
    fn request_id_monotonically_increasing() {
        let generator = RequestIdGenerator::new();
        let id1 = generator.next_id();
        let id2 = generator.next_id();
        // The counter portion should be different.
        assert_ne!(id1, id2);
    }

    // -- RateLimiter -----------------------------------------------------------

    #[test]
    fn rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new(100);
        for _ in 0..10 {
            assert!(limiter.is_allowed("127.0.0.1"));
        }
    }

    #[test]
    fn rate_limiter_different_ips_independent() {
        let limiter = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(limiter.is_allowed("10.0.0.1"));
        }
        // Different IP should still be allowed.
        assert!(limiter.is_allowed("10.0.0.2"));
    }

    // -- RequestDeduplicator with path -----------------------------------------

    #[test]
    fn dedup_same_body_different_path_is_not_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate_with_path("/v1/messages", b"hello"));
        // Same body, different path -> not a duplicate.
        assert!(!dedup.is_duplicate_with_path("/v1/chat/completions", b"hello"));
    }

    #[test]
    fn dedup_same_body_same_path_is_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate_with_path("/v1/messages", b"hello"));
        assert!(dedup.is_duplicate_with_path("/v1/messages", b"hello"));
    }

    #[test]
    fn dedup_custom_window() {
        let dedup = RequestDeduplicator::with_window_ms(1000);
        assert!(!dedup.is_duplicate(b"hello"));
        assert!(dedup.is_duplicate(b"hello"));
    }

    #[test]
    fn dedup_zero_window_is_disabled() {
        let dedup = RequestDeduplicator::with_window_ms(0);
        assert!(!dedup.is_duplicate(b"hello"));
        assert!(!dedup.is_duplicate(b"hello"));
    }

    // -- get_client_ip ---------------------------------------------------------

    #[test]
    fn client_ip_trust_forwarded_headers_false_ignores_headers() {
        use axum::http::HeaderMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let connect_info = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        // With trust_forwarded_headers = false, should use connection info.
        let ip = super::get_client_ip(&headers, Some(&connect_info), false);
        assert_eq!(
            ip, "127.0.0.1",
            "should ignore X-Forwarded-For when trust=false"
        );
    }

    #[test]
    fn client_ip_trust_forwarded_headers_true_uses_headers() {
        use axum::http::HeaderMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let connect_info = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        // With trust_forwarded_headers = true, should use X-Forwarded-For.
        let ip = super::get_client_ip(&headers, Some(&connect_info), true);
        assert_eq!(ip, "1.2.3.4", "should use X-Forwarded-For when trust=true");
    }

    #[test]
    fn rate_limiter_zero_limit_is_disabled() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1_000 {
            assert!(limiter.is_allowed("127.0.0.1"));
        }
    }
}
