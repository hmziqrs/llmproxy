//! Runtime metrics for the LLM proxy.
//!
//! All counters are lock-free ([`std::sync::atomic::AtomicI64`]). The only
//! mutex-guarded state is the latency ring-buffer (last 1 000 samples) and
//! the per-provider-model request counter map.
//!
//! # Ordering
//!
//! All atomic operations use [`std::sync::atomic::Ordering::Relaxed`] which is correct here
//! because the counters are monotonically increasing, used only for
//! observability, and no synchronization invariants depend on their
//! relative ordering.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

/// Maximum number of latency samples retained in the ring-buffer.
const LATENCY_CAP: usize = 1000;

/// Maximum number of distinct `"{provider}:{model}"` keys retained in
/// [`Metrics::model_counts`].
///
/// The `model` axis is attacker-controlled (it arrives from the inbound client
/// request and, when catalog enforcement is off -- the default --, is not
/// canonicalized before keying). Without a cap, a client sending a unique model
/// string per request could grow this map without bound for the lifetime of the
/// process. Once this many distinct keys exist, any further new pair is folded
/// into the [`MODEL_COUNTS_OVERFLOW`] aggregate bucket instead of allocating a
/// new entry, bounding memory while still reporting the total request count.
/// The hard bound on map size is therefore [`MODEL_COUNTS_CAP`] real keys plus
/// at most one lazily-created overflow bucket.
const MODEL_COUNTS_CAP: usize = 1024;

/// Aggregate bucket key used once [`MODEL_COUNTS_CAP`] distinct keys have been
/// recorded. Chosen so it cannot collide with a real `"{provider}:{model}"`
/// key: the provider segment of a real key is URL-path-validated against
/// `^[a-z0-9_-]+$`, so a leading `~` (not in that class) makes this key
/// structurally unreachable from any request input. (A prior sentinel
/// `__other__:` was collidable because `_` is a legal provider-name character,
/// so a configured provider literally named `__other__` serving an empty model
/// would alias the overflow bucket.)
const MODEL_COUNTS_OVERFLOW: &str = "~overflow:";

/// Separator used in the composite metrics key: `"{provider}:{model}"`.
///
/// The `:` delimiter is chosen because provider names are restricted to
/// `[a-z0-9_-]+` and model IDs cannot contain `:`, preventing key collisions.
const KEY_SEPARATOR: &str = ":";

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Thread-safe metrics collector.
///
/// Internally uses [`AtomicI64`] counters for hot-path increments and
/// [`Mutex`] only for the latency ring-buffer and provider-model map.
#[derive(Debug)]
pub struct Metrics {
    requests_received: AtomicI64,
    requests_streamed: AtomicI64,
    requests_success: AtomicI64,
    requests_failed: AtomicI64,
    upstream_calls: AtomicI64,
    rate_limited: AtomicI64,
    deduplicated: AtomicI64,
    client_cancelled: AtomicI64,
    /// Ring-buffer holding the last [`LATENCY_CAP`] latency samples.
    latencies: Mutex<VecDeque<Duration>>,
    /// Per-provider-model request counts. Key format: `"{provider}:{model}"`.
    model_counts: Mutex<HashMap<String, i64>>,
}

impl Metrics {
    /// Create a new, zeroed [`Metrics`] instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            requests_received: AtomicI64::new(0),
            requests_streamed: AtomicI64::new(0),
            requests_success: AtomicI64::new(0),
            requests_failed: AtomicI64::new(0),
            upstream_calls: AtomicI64::new(0),
            rate_limited: AtomicI64::new(0),
            deduplicated: AtomicI64::new(0),
            client_cancelled: AtomicI64::new(0),
            latencies: Mutex::new(VecDeque::with_capacity(LATENCY_CAP)),
            model_counts: Mutex::new(HashMap::new()),
        }
    }

    /// Record an incoming request.
    ///
    /// If `streaming` is `true`, both `requests_received` and
    /// `requests_streamed` are incremented.
    pub fn record_request(&self, streaming: bool) {
        self.requests_received.fetch_add(1, Ordering::Relaxed);
        if streaming {
            self.requests_streamed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a successful upstream response.
    ///
    /// Increments `requests_success` and `upstream_calls`, stores the
    /// `latency` sample, and bumps the per-`provider`/`model` counter.
    pub fn record_success(&self, provider: &str, model: &str, latency: Duration) {
        self.requests_success.fetch_add(1, Ordering::Relaxed);
        self.upstream_calls.fetch_add(1, Ordering::Relaxed);

        // Store latency sample (ring-buffer), then release the latencies lock
        // before touching model_counts so the two independent maps are not
        // serialized by a single held guard. Recover from poison to preserve
        // data from panicked threads. Renaming the inner binding from `buf` to
        // `latencies` avoids shadowing the thread-local `buf` in the closure
        // below (audit LOW-9).
        {
            let mut latencies = self.latencies.lock().unwrap_or_else(|e| e.into_inner());
            if latencies.len() >= LATENCY_CAP {
                latencies.pop_front();
            }
            latencies.push_back(latency);
        }

        // Bump per-provider-model counter.
        //
        // The composite key is built into a thread-local reusable buffer so no
        // fresh `String` is heap-allocated per call. On the hot path (provider/
        // model pair already tracked) the buffer is only borrowed for a `get`
        // lookup; the cold insert path clones the buffer contents into an owned
        // key exactly once per distinct pair (audit LOW-7).
        thread_local! {
            static KEY_BUF: std::cell::RefCell<String> =
                std::cell::RefCell::new(String::with_capacity(64));
        }
        let mut map = self.model_counts.lock().unwrap_or_else(|e| e.into_inner());
        KEY_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            buf.clear();
            buf.push_str(provider);
            buf.push_str(KEY_SEPARATOR);
            buf.push_str(model);
            if let Some(count) = map.get_mut(buf.as_str()) {
                *count += 1;
            } else if map.len() < MODEL_COUNTS_CAP {
                // Cold path: first sighting of this pair — pay for one owned key.
                *map.entry(buf.clone()).or_insert(0) += 1;
            } else {
                // The map is at capacity (defending against attacker-controlled,
                // unbounded model strings -- see [`MODEL_COUNTS_CAP`]). Fold this
                // and any further distinct pairs into the aggregate overflow
                // bucket instead of allocating a new key. The bucket itself is
                // lazily created the first time it is needed.
                *map.entry(MODEL_COUNTS_OVERFLOW.to_owned()).or_insert(0) += 1;
            }
        });
    }

    /// Record a failed request.
    ///
    /// Increments `requests_failed` and `upstream_calls`.
    ///
    /// Reserve this for genuine upstream/decode failures. A client-initiated
    /// disconnect (the client went away mid-stream, or the timeout layer tore
    /// the connection down) is **not** a failure of the upstream call and must
    /// be recorded via [`Metrics::record_client_cancel`] instead, so that the
    /// error-rate SLO is not corrupted by users pressing "stop" (audit MEDIUM-5).
    pub fn record_failure(&self) {
        self.requests_failed.fetch_add(1, Ordering::Relaxed);
        self.upstream_calls.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a client-initiated cancellation.
    ///
    /// Distinct from [`Metrics::record_failure`]: this does not touch
    /// `requests_failed` or `upstream_calls`, so client "stop" presses and
    /// timeout teardowns do not inflate the error-rate SLO (audit MEDIUM-5).
    pub fn record_client_cancel(&self) {
        self.client_cancelled.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a rate-limited request.
    pub fn record_rate_limited(&self) {
        self.rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a deduplicated (cache-hit) request.
    pub fn record_deduplicated(&self) {
        self.deduplicated.fetch_add(1, Ordering::Relaxed);
    }

    /// Take an approximate point-in-time snapshot of all counters.
    ///
    /// Individual fields are each loaded atomically, but the overall
    /// snapshot is **approximately** consistent rather than transactional:
    /// between loading the first counter and the last, other threads may
    /// have mutated state. This is acceptable for observability use cases.
    pub fn get_snapshot(&self) -> Snapshot {
        let requests_received = self.requests_received.load(Ordering::Relaxed);
        let requests_streamed = self.requests_streamed.load(Ordering::Relaxed);
        let requests_success = self.requests_success.load(Ordering::Relaxed);
        let requests_failed = self.requests_failed.load(Ordering::Relaxed);
        let upstream_calls = self.upstream_calls.load(Ordering::Relaxed);
        let rate_limited = self.rate_limited.load(Ordering::Relaxed);
        let deduplicated = self.deduplicated.load(Ordering::Relaxed);
        let client_cancelled = self.client_cancelled.load(Ordering::Relaxed);

        let latencies = self
            .latencies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();

        let model_counts = self
            .model_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();

        Snapshot {
            requests_received,
            requests_streamed,
            requests_success,
            requests_failed,
            upstream_calls,
            rate_limited,
            deduplicated,
            client_cancelled,
            latencies,
            model_counts,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// An approximate point-in-time copy of all metric counters.
///
/// Individual fields are each loaded atomically but the snapshot as a whole
/// is not a transactional view. This is acceptable for observability.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Total requests received (streaming + non-streaming).
    pub requests_received: i64,
    /// Subset of received requests that used streaming.
    pub requests_streamed: i64,
    /// Requests that completed successfully.
    pub requests_success: i64,
    /// Requests that failed.
    pub requests_failed: i64,
    /// Total calls made to upstream providers.
    pub upstream_calls: i64,
    /// Requests rejected by the rate limiter.
    pub rate_limited: i64,
    /// Requests that were deduplicated before reaching upstream.
    pub deduplicated: i64,
    /// Requests whose client disconnected (or were torn down by the timeout
    /// layer) mid-stream. Tracked separately from `requests_failed` so client
    /// "stop" actions do not corrupt the error-rate SLO (audit MEDIUM-5).
    pub client_cancelled: i64,
    /// Collected latency samples (up to 1 000 entries).
    pub latencies: Vec<Duration>,
    /// Per-provider-model request counts. Key format: `"{provider}:{model}"`.
    pub model_counts: HashMap<String, i64>,
}

impl Snapshot {
    /// Compute the latency at an arbitrary percentile (0..=100).
    ///
    /// Uses the "exclusive" interpolation method common in monitoring systems:
    /// `rank = pct/100 * (n + 1)`, then clamped to `[0, n-1]`.
    ///
    /// Returns [`Duration::ZERO`] for an empty sample set.
    pub fn calculate_percentile(&self, pct: f64) -> Duration {
        percentile(&self.latencies, pct)
    }

    /// Compute the p95 latency from the collected samples.
    ///
    /// Returns [`Duration::ZERO`] when no samples have been recorded.
    pub fn calculate_p95(&self) -> Duration {
        percentile(&self.latencies, 95.0)
    }

    /// Compute the p99 latency from the collected samples.
    ///
    /// Returns [`Duration::ZERO`] when no samples have been recorded.
    pub fn calculate_p99(&self) -> Duration {
        percentile(&self.latencies, 99.0)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the latency at the given percentile (0..=100).
///
/// Uses the "exclusive" interpolation method common in monitoring systems:
/// `rank = pct/100 * (n + 1)`, then clamped to `[0, n-1]`.
///
/// Returns [`Duration::ZERO`] for an empty slice.
fn percentile(samples: &[Duration], pct: f64) -> Duration {
    // Clamp the input unconditionally so an out-of-range `pct` (e.g. a caller
    // bug passing 150.0 or a negative) cannot silently yield the wrong sample.
    // This clamp is the hard guard that survives in both debug and release
    // builds; `calculate_percentile_clamps_out_of_range_input` exercises it.
    let pct = pct.clamp(0.0, 100.0);

    if samples.is_empty() {
        return Duration::ZERO;
    }

    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();

    let n = sorted.len();
    // Nearest-rank (exclusive) method: rank = pct/100 * (n + 1), 1-based.
    let rank = (pct / 100.0) * (n as f64 + 1.0);
    // Convert to 0-based index, clamped to valid range.
    #[expect(
        clippy::cast_sign_loss,
        reason = "`pct` is clamped to 0.0..=100.0 and `n >= 1`, so `rank = pct/100 * (n+1)` is always >= 0.0 and the cast cannot lose a sign"
    )]
    let idx = (rank.floor() as usize).saturating_sub(1).min(n - 1);
    sorted[idx]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_all_zero() {
        let m = Metrics::new();
        let snap = m.get_snapshot();
        assert_eq!(snap.requests_received, 0);
        assert_eq!(snap.requests_streamed, 0);
        assert_eq!(snap.requests_success, 0);
        assert_eq!(snap.requests_failed, 0);
        assert_eq!(snap.upstream_calls, 0);
        assert_eq!(snap.rate_limited, 0);
        assert_eq!(snap.deduplicated, 0);
        assert_eq!(snap.client_cancelled, 0);
        assert!(snap.latencies.is_empty());
        assert!(snap.model_counts.is_empty());
    }

    #[test]
    fn record_request_non_streaming() {
        let m = Metrics::new();
        m.record_request(false);
        let snap = m.get_snapshot();
        assert_eq!(snap.requests_received, 1);
        assert_eq!(snap.requests_streamed, 0);
    }

    #[test]
    fn record_request_streaming() {
        let m = Metrics::new();
        m.record_request(true);
        let snap = m.get_snapshot();
        assert_eq!(snap.requests_received, 1);
        assert_eq!(snap.requests_streamed, 1);
    }

    #[test]
    fn record_success_tracks_latency_and_provider_model() {
        let m = Metrics::new();
        m.record_success("openai", "gpt-4o", Duration::from_millis(120));
        m.record_success("openai", "gpt-4o", Duration::from_millis(80));
        m.record_success("anthropic", "claude-3", Duration::from_millis(200));

        let snap = m.get_snapshot();
        assert_eq!(snap.requests_success, 3);
        assert_eq!(snap.upstream_calls, 3);
        assert_eq!(snap.latencies.len(), 3);
        assert_eq!(snap.model_counts.get("openai:gpt-4o"), Some(&2));
        assert_eq!(snap.model_counts.get("anthropic:claude-3"), Some(&1));
    }

    #[test]
    fn record_success_same_model_different_providers_are_distinct() {
        let m = Metrics::new();
        m.record_success("provider-a", "gpt-4o", Duration::from_millis(100));
        m.record_success("provider-b", "gpt-4o", Duration::from_millis(200));

        let snap = m.get_snapshot();
        assert_eq!(snap.model_counts.get("provider-a:gpt-4o"), Some(&1));
        assert_eq!(snap.model_counts.get("provider-b:gpt-4o"), Some(&1));
        assert_eq!(snap.model_counts.len(), 2);
    }

    #[test]
    fn record_failure_increments_counters() {
        let m = Metrics::new();
        m.record_failure();
        m.record_failure();
        let snap = m.get_snapshot();
        assert_eq!(snap.requests_failed, 2);
        assert_eq!(snap.upstream_calls, 2);
    }

    #[test]
    fn record_client_cancel_is_distinct_from_failure() {
        let m = Metrics::new();
        m.record_client_cancel();
        m.record_client_cancel();
        m.record_failure();
        let snap = m.get_snapshot();
        assert_eq!(snap.client_cancelled, 2);
        assert_eq!(snap.requests_failed, 1);
        assert_eq!(
            snap.upstream_calls, 1,
            "client cancellation must NOT inflate upstream_calls / error-rate SLO"
        );
    }

    #[test]
    fn record_rate_limited_and_deduplicated() {
        let m = Metrics::new();
        m.record_rate_limited();
        m.record_rate_limited();
        m.record_rate_limited();
        m.record_deduplicated();

        let snap = m.get_snapshot();
        assert_eq!(snap.rate_limited, 3);
        assert_eq!(snap.deduplicated, 1);
    }

    #[test]
    fn latency_ring_buffer_capped_at_1000() {
        let m = Metrics::new();
        for i in 0..1050 {
            m.record_success("p", "model", Duration::from_millis(i));
        }

        let snap = m.get_snapshot();
        assert_eq!(snap.latencies.len(), LATENCY_CAP);
        // The oldest 50 samples should have been evicted; the buffer should
        // start at sample index 50.
        assert_eq!(snap.latencies[0], Duration::from_millis(50));
        assert_eq!(snap.latencies[LATENCY_CAP - 1], Duration::from_millis(1049));
    }

    #[test]
    fn p95_p99_empty() {
        let snap = Snapshot::default();
        assert_eq!(snap.calculate_p95(), Duration::ZERO);
        assert_eq!(snap.calculate_p99(), Duration::ZERO);
    }

    #[test]
    fn p95_p99_on_known_samples() {
        // 100 samples from 0ms..99ms.
        let latencies: Vec<Duration> = (0..100).map(Duration::from_millis).collect();

        let snap = Snapshot {
            latencies,
            ..Snapshot::default()
        };

        // p95: rank = 0.95 * (100+1) = 95.95 -> floor 95 -> idx 94 -> 94ms
        assert_eq!(snap.calculate_p95(), Duration::from_millis(94));
        // p99: rank = 0.99 * (100+1) = 99.99 -> floor 99 -> idx 98 -> 98ms
        assert_eq!(snap.calculate_p99(), Duration::from_millis(98));
    }

    #[test]
    fn snapshot_is_clone_and_debug() {
        let m = Metrics::new();
        m.record_success("p", "test", Duration::from_millis(10));
        let snap = m.get_snapshot();

        let cloned = snap.clone();
        assert_eq!(cloned.requests_success, 1);

        let _debug_str = format!("{snap:?}");
    }

    #[test]
    fn concurrent_access_does_not_panic() {
        use std::sync::Arc;
        use std::thread;

        fn record_one(m: &Metrics, i: u64) {
            m.record_request(i % 2 == 0);
            if i % 3 == 0 {
                m.record_success("p", "model-a", Duration::from_micros(i));
            } else {
                m.record_failure();
            }
            if i % 10 == 0 {
                m.record_rate_limited();
            }
            if i % 7 == 0 {
                m.record_deduplicated();
            }
        }

        let m = Arc::new(Metrics::new());
        let mut handles = vec![];

        for _ in 0..4 {
            let m = Arc::clone(&m);
            handles.push(thread::spawn(move || {
                for i in 0..500 {
                    record_one(&m, i);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let snap = m.get_snapshot();
        // 4 threads x 500 iterations = 2000 requests received.
        assert_eq!(snap.requests_received, 2000);
        assert_eq!(snap.requests_success + snap.requests_failed, 2000);
    }

    #[test]
    fn default_trait() {
        let m = Metrics::default();
        let snap = m.get_snapshot();
        assert_eq!(snap.requests_received, 0);
    }

    #[test]
    fn snapshot_default_is_zeroed() {
        let snap = Snapshot::default();
        assert_eq!(snap.requests_received, 0);
        assert_eq!(snap.latencies.len(), 0);
        assert!(snap.model_counts.is_empty());
    }

    #[test]
    fn snapshot_partial_eq_works() {
        let a = Snapshot::default();
        let b = Snapshot::default();
        assert_eq!(a, b);
    }

    #[test]
    fn p95_single_sample() {
        let snap = Snapshot {
            latencies: vec![Duration::from_millis(42)],
            ..Snapshot::default()
        };
        assert_eq!(snap.calculate_p95(), Duration::from_millis(42));
        assert_eq!(snap.calculate_p99(), Duration::from_millis(42));
    }

    #[test]
    fn p95_two_samples() {
        let snap = Snapshot {
            latencies: vec![Duration::from_millis(10), Duration::from_millis(100)],
            ..Snapshot::default()
        };
        // p95: rank = 0.95 * 3 = 2.85 -> floor 2 -> idx 1 -> 100ms
        assert_eq!(snap.calculate_p95(), Duration::from_millis(100));
        // p99: rank = 0.99 * 3 = 2.97 -> floor 2 -> idx 1 -> 100ms
        assert_eq!(snap.calculate_p99(), Duration::from_millis(100));
    }

    #[test]
    fn calculate_percentile_public_api() {
        let snap = Snapshot {
            latencies: (0..100).map(Duration::from_millis).collect(),
            ..Snapshot::default()
        };
        assert_eq!(snap.calculate_percentile(50.0), Duration::from_millis(49));
    }

    #[test]
    fn calculate_percentile_clamps_out_of_range_input() {
        // Out-of-range `pct` must be clamped to [0, 100] rather than silently
        // returning the wrong sample (the debug_assert compiles out in release).
        let snap = Snapshot {
            latencies: (0..100).map(Duration::from_millis).collect(),
            ..Snapshot::default()
        };
        // pct > 100 clamps to 100 -> max sample (99ms).
        assert_eq!(snap.calculate_percentile(150.0), Duration::from_millis(99));
        assert_eq!(snap.calculate_percentile(100.0), Duration::from_millis(99));
        // pct < 0 clamps to 0 -> min sample (0ms).
        assert_eq!(snap.calculate_percentile(-42.0), Duration::from_millis(0));
        assert_eq!(snap.calculate_percentile(0.0), Duration::from_millis(0));
    }

    #[test]
    fn get_snapshot_concurrent_with_writes() {
        use std::sync::Arc;
        use std::thread;

        let m = Arc::new(Metrics::new());
        let mut handles = vec![];

        for _ in 0..2 {
            let m = Arc::clone(&m);
            handles.push(thread::spawn(move || {
                for i in 0..200 {
                    m.record_success("p", "model", Duration::from_micros(i));
                }
            }));
        }

        let m_reader = Arc::clone(&m);
        let reader = thread::spawn(move || {
            for _ in 0..100 {
                let snap = m_reader.get_snapshot();
                assert!(snap.requests_success >= 0);
                assert!(snap.latencies.len() <= LATENCY_CAP);
            }
        });

        for h in handles {
            h.join().unwrap();
        }
        reader.join().unwrap();
    }

    #[test]
    fn key_separator_does_not_collide_with_provider_names() {
        // Provider names are restricted to [a-z0-9_-]+ so `:` separator
        // cannot appear in provider or model names.
        let m = Metrics::new();
        m.record_success("provider-a", "model-x", Duration::from_millis(10));
        m.record_success("provider-a", "model-y", Duration::from_millis(20));
        let snap = m.get_snapshot();
        assert_eq!(snap.model_counts.len(), 2);
        assert!(snap.model_counts.contains_key("provider-a:model-x"));
        assert!(snap.model_counts.contains_key("provider-a:model-y"));
    }

    #[test]
    fn model_counts_capped_with_overflow_bucket() {
        // The model axis is attacker-controlled, so the distinct-key map must be
        // bounded. Once MODEL_COUNTS_CAP distinct pairs have been recorded, any
        // further new pair is folded into the `~overflow:` aggregate bucket
        // instead of allocating a new entry. The bound is MODEL_COUNTS_CAP real
        // keys plus at most one lazily-created overflow bucket.
        let m = Metrics::new();

        // Fill the map to capacity with distinct pairs.
        for i in 0..MODEL_COUNTS_CAP {
            m.record_success("p", &format!("model-{i}"), Duration::from_micros(i as u64));
        }
        let snap = m.get_snapshot();
        assert_eq!(
            snap.model_counts.len(),
            MODEL_COUNTS_CAP,
            "map should be exactly at capacity before overflow"
        );
        assert!(
            !snap.model_counts.contains_key(MODEL_COUNTS_OVERFLOW),
            "overflow bucket must not exist before capacity is reached"
        );

        // Further distinct pairs spill into the aggregate bucket. The bucket is
        // created on first spill, so the map grows by exactly one entry.
        m.record_success("p", "overflow-1", Duration::from_micros(0));
        m.record_success("p", "overflow-2", Duration::from_micros(0));
        let snap = m.get_snapshot();
        assert_eq!(
            snap.model_counts.len(),
            MODEL_COUNTS_CAP + 1,
            "only the overflow bucket may be added once capacity is reached"
        );
        assert_eq!(
            snap.model_counts.get(MODEL_COUNTS_OVERFLOW),
            Some(&2),
            "spilled counts should accumulate in the overflow bucket"
        );

        // More distinct pairs must NOT grow the map further.
        m.record_success("p", "overflow-3", Duration::from_micros(0));
        let snap = m.get_snapshot();
        assert_eq!(
            snap.model_counts.len(),
            MODEL_COUNTS_CAP + 1,
            "subsequent overflow must not grow the map"
        );
        assert_eq!(snap.model_counts.get(MODEL_COUNTS_OVERFLOW), Some(&3));

        // An already-tracked pair must still increment its own entry (hot path),
        // not the overflow bucket.
        m.record_success("p", "model-0", Duration::from_micros(0));
        let snap = m.get_snapshot();
        assert_eq!(
            snap.model_counts.get("p:model-0"),
            Some(&2),
            "existing key must still be incremented after overflow began"
        );
        assert_eq!(
            snap.model_counts.get(MODEL_COUNTS_OVERFLOW),
            Some(&3),
            "overflow bucket must not change for an existing key"
        );
    }
}
