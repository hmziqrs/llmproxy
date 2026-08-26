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

/// Maximum number of in-flight deduplication entries. Prevents unbounded
/// memory growth from unique-body requests.
const MAX_DEDUP_ENTRIES: usize = 10_000;

/// Maximum body size (in bytes) eligible for dedup tracking. Bodies larger
/// than this are never hashed or tracked to avoid expensive SHA-256 computation
/// on large payloads and prevent memory pressure from storing many hash entries.
/// 64 KiB is chosen as a reasonable threshold: most LLM API requests with
/// moderate context fit within this limit, while very large file uploads or
/// multi-turn conversations with huge contexts are excluded from dedup.
const MAX_DEDUP_BODY_SIZE: usize = 64 * 1024;

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
    /// SHA-256 raw digest -> insertion time. Keyed on the 32-byte digest
    /// (`[u8; 32]` is `Copy + Eq + Hash`) rather than a 64-char hex `String`
    /// to avoid a per-request allocation and formatting pass (audit LOW-8).
    in_flight: Mutex<HashMap<[u8; 32], Instant>>,
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
    ///
    /// # Deprecation
    ///
    /// Prefer [`Self::is_duplicate_with_path`] which includes the request path in the
    /// hash to distinguish requests to different protocol endpoints.
    #[deprecated(note = "use is_duplicate_with_path instead")]
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
        // Skip dedup for large bodies to avoid expensive hashing and memory pressure.
        if body.len() > MAX_DEDUP_BODY_SIZE {
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

        // Skip dedup tracking when at capacity to bound memory usage.
        if map.len() < MAX_DEDUP_ENTRIES {
            map.insert(hash, now);
        }
        false
    }

    /// Compute the SHA-256 digest of the request path and body.
    ///
    /// Returns the raw 32-byte digest directly (rather than a 64-char hex
    /// `String`) since `[u8; 32]` is `Copy + Eq + Hash` and serves as the
    /// `in_flight` key without any formatting pass or heap allocation
    /// (audit LOW-8).
    fn hash_path_body(path: &str, body: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        hasher.update(body);
        hasher.finalize().into()
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

/// Maximum number of per-IP rate-limit buckets. Prevents unbounded memory
/// growth from spoofed or diverse source IPs.
const MAX_RATE_LIMIT_BUCKETS: usize = 100_000;

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
/// requests per minute. Pruning of stale entries occurs periodically
/// (every `PRUNE_INTERVAL` requests) rather than on every call to reduce
/// per-request overhead under high load.
#[derive(Debug)]
pub struct RateLimiter {
    /// Per-IP token buckets.
    buckets: Mutex<HashMap<String, ClientTokenBucket>>,
    /// Maximum requests per minute per client.
    max_requests_per_minute: f64,
    /// Counter for periodic pruning. Pruning runs every `PRUNE_INTERVAL` requests.
    prune_counter: AtomicU64,
}

/// Number of requests between prune sweeps. Trade-off: lower values prune
/// more aggressively but add overhead; higher values reduce overhead but
/// allow stale entries to linger longer.
const PRUNE_INTERVAL: u64 = 128;

/// Idle eviction threshold for per-IP rate-limit buckets (5 minutes).
///
/// Buckets whose last refill was more than this duration ago are evicted
/// during periodic pruning to prevent unbounded memory growth.
const RATE_LIMITER_IDLE_EVICT_SECS: u64 = 300;

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// `max_requests_per_minute` is the maximum number of requests allowed
    /// per client IP per minute. `0` disables rate limiting.
    ///
    /// # Panics
    ///
    /// Does not panic, but extremely large values (e.g. `u32::MAX`) create
    /// per-IP buckets with billions of tokens. Operators should choose
    /// reasonable values (e.g. 100--100_000 RPM) to avoid pathological
    /// memory usage from unbounded per-IP bucket allocation.
    pub fn new(max_requests_per_minute: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_requests_per_minute: max_requests_per_minute as f64,
            prune_counter: AtomicU64::new(0),
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

        // Periodic pruning: evict stale entries every PRUNE_INTERVAL requests
        // to prevent unbounded memory growth without paying the cost on every call.
        if self.prune_counter.fetch_add(1, Ordering::Relaxed) % PRUNE_INTERVAL == 0 {
            let now = std::time::Instant::now();
            buckets.retain(|_, bucket| {
                now.duration_since(bucket.last_refill)
                    < std::time::Duration::from_secs(RATE_LIMITER_IDLE_EVICT_SECS)
            });
        }

        // Refill rate: tokens per second.
        let refill_rate = self.max_requests_per_minute / 60.0;

        // Single-lookup hot path: for a known IP we mutate the existing bucket
        // in place without allocating a key `String`. Only on a miss do we
        // perform the capacity check and a second (entry) probe that allocates.
        // This avoids the redundant `contains_key` + `entry` double hash probe
        // and the unconditional `to_owned()` on every call (audit LOW-6).
        if let Some(bucket) = buckets.get_mut(client_ip) {
            return bucket.try_consume(refill_rate);
        }

        // New IP: reject if inserting it would exceed the bucket cap. Bounds
        // memory growth from spoofed or diverse source IPs.
        if buckets.len() >= MAX_RATE_LIMIT_BUCKETS {
            return false;
        }

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
    ///
    /// The ID format is `req-{unix_seconds}-{monotonic_counter}`.
    ///
    /// # Clock fallback
    ///
    /// If `SystemTime::now()` is before `UNIX_EPOCH` (nearly impossible on
    /// modern systems), the timestamp falls back to 0. The monotonic counter
    /// still guarantees uniqueness within the process.
    pub fn next_id(&self) -> String {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "system clock is before UNIX epoch; using 0 as request ID timestamp");
                0
            });
        // Ordering::Relaxed is sufficient: the counter only needs to be
        // monotonic within this process. No other memory depends on the
        // counter value for synchronization.
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("req-{unix}-{counter}")
    }
}

impl Default for RequestIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Typed request identifier carried in request extensions.
///
/// Inserted by an outermost `middleware::from_fn` layer so that the
/// `TraceLayer` span (`make_span_with`) and the route handler share a single
/// id. This keeps the id that appears in the `x-request-id` response header and
/// in handler log events identical to the id stamped on the request's tracing
/// span, restoring end-to-end span/event correlation (audit MEDIUM-1).
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
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
///
/// When `trust_forwarded_headers` is true, the leftmost value from
/// `X-Forwarded-For` or the value from `X-Real-Ip` is used, but only after
/// validating it parses as a legitimate [`std::net::IpAddr`]. Invalid or
/// spoofed values are silently ignored and the connection-info fallback is
/// used instead.
///
/// # Trust model
///
/// The **leftmost** `X-Forwarded-For` entry is trusted as the client address.
/// This is only correct behind a **single** reverse proxy that strips or
/// overwrites any inbound `X-Forwarded-For` before appending its own hop. With
/// multiple untrusted hops the leftmost entry is attacker-controllable (a
/// client may prepend arbitrary IPs), so the extracted address must not be
/// trusted for authentication, rate-limit bypass, or audit logging. The IpAddr
/// parse check defends against malformed garbage, not against well-formed
/// spoofed values.
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
                    if trimmed.parse::<std::net::IpAddr>().is_ok() {
                        return trimmed.to_owned();
                    }
                }
            }
        }

        // Check X-Real-Ip.
        if let Some(xri) = headers.get("x-real-ip") {
            if let Ok(val) = xri.to_str() {
                let trimmed = val.trim();
                if trimmed.parse::<std::net::IpAddr>().is_ok() {
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
        assert!(!dedup.is_duplicate_with_path("", b"hello"));
    }

    #[test]
    fn dedup_same_body_is_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate_with_path("", b"hello"));
        assert!(dedup.is_duplicate_with_path("", b"hello"));
    }

    #[test]
    fn dedup_different_body_is_not_duplicate() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate_with_path("", b"hello"));
        assert!(!dedup.is_duplicate_with_path("", b"world"));
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
        assert!(!dedup.is_duplicate_with_path("", b"hello"));
        assert!(dedup.is_duplicate_with_path("", b"hello"));
    }

    #[test]
    fn dedup_zero_window_is_disabled() {
        let dedup = RequestDeduplicator::with_window_ms(0);
        assert!(!dedup.is_duplicate_with_path("", b"hello"));
        assert!(!dedup.is_duplicate_with_path("", b"hello"));
    }

    /// Exercise the deprecated path-less shim so the public entry point cannot
    /// break silently. `#[expect(deprecated)]` (rather than `#[allow]`) surfaces
    /// an `unfulfilled_lint_expectation` warning the day the shim is removed.
    #[test]
    #[expect(deprecated, reason = "is_duplicate shim is exercised directly here")]
    fn dedup_path_less_shim_still_works() {
        let dedup = RequestDeduplicator::new();
        assert!(!dedup.is_duplicate(b"hello"));
        // Identical body seen twice under the empty path is still a duplicate.
        assert!(dedup.is_duplicate(b"hello"));
    }

    // -- get_client_ip ---------------------------------------------------------

    #[test]
    fn client_ip_trust_forwarded_headers_false_ignores_headers() {
        use axum::http::HeaderMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let connect_info = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

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

    #[test]
    fn rate_limiter_rejects_requests_at_the_limit() {
        // With 1 RPM, the first request should be allowed and the second rejected.
        let limiter = RateLimiter::new(1);
        assert!(
            limiter.is_allowed("10.0.0.1"),
            "first request should be allowed"
        );
        assert!(
            !limiter.is_allowed("10.0.0.1"),
            "second request should be rejected at 1 RPM limit"
        );
    }

    #[test]
    fn dedup_large_body_is_not_tracked() {
        let dedup = RequestDeduplicator::with_window_ms(5000);
        let large_body = vec![b'x'; 65 * 1024]; // 65 KiB > 64 KiB cap
        // Large bodies should never be tracked as duplicates.
        assert!(!dedup.is_duplicate_with_path("/v1/messages", &large_body));
        assert!(
            !dedup.is_duplicate_with_path("/v1/messages", &large_body),
            "large body should not be tracked for dedup"
        );
    }

    #[test]
    fn dedup_body_at_cap_is_still_tracked() {
        let dedup = RequestDeduplicator::with_window_ms(5000);
        let body_at_cap = vec![b'x'; 64 * 1024]; // Exactly at 64 KiB cap
        assert!(!dedup.is_duplicate_with_path("/v1/messages", &body_at_cap));
        assert!(
            dedup.is_duplicate_with_path("/v1/messages", &body_at_cap),
            "body at exactly the cap should still be tracked"
        );
    }

    // -- Multi-value X-Forwarded-For (finding 24) ------------------------------

    #[test]
    fn client_ip_multi_value_xff_picks_leftmost() {
        use axum::http::HeaderMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        let connect_info = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let ip = super::get_client_ip(&headers, Some(&connect_info), true);
        assert_eq!(
            ip, "1.2.3.4",
            "should pick the leftmost IP from multi-value XFF"
        );
    }

    // -- get_client_ip returns 'unknown' (finding 26) --------------------------

    #[test]
    fn client_ip_returns_unknown_when_no_connect_info_and_no_headers() {
        use axum::http::HeaderMap;

        let headers = HeaderMap::new();
        let ip = super::get_client_ip(&headers, None, false);
        assert_eq!(
            ip, "unknown",
            "should return 'unknown' when no IP source is available"
        );
    }

    #[test]
    fn client_ip_trusted_xff_with_invalid_ip_falls_through() {
        use axum::http::HeaderMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        let connect_info = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 8080);

        let ip = super::get_client_ip(&headers, Some(&connect_info), true);
        assert_eq!(
            ip, "10.0.0.1",
            "invalid XFF value should fall through to connect_info"
        );
    }

    #[test]
    fn client_ip_x_real_ip_is_used_when_xff_absent() {
        use axum::http::HeaderMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "9.8.7.6".parse().unwrap());
        let connect_info = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let ip = super::get_client_ip(&headers, Some(&connect_info), true);
        assert_eq!(ip, "9.8.7.6", "should use X-Real-Ip when XFF is absent");
    }
}
