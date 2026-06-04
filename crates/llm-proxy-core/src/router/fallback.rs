//! Circuit breaker and model-fallback handling.
//!
//! Ported from the Go reference `internal/router/fallback.go`. Each upstream
//! model gets its own [`CircuitBreaker`] so that repeated failures only block
//! the offending model while remaining models stay available. The
//! [`FallbackHandler`] walks a fallback chain, skipping models whose circuit is
//! open, until one succeeds or every model has been tried.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::config::ModelConfig;

// ---------------------------------------------------------------------------
// CircuitState
// ---------------------------------------------------------------------------

/// Possible states of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation -- requests are allowed.
    Closed,
    /// Probing -- a limited number of test requests are allowed.
    HalfOpen,
    /// Failing fast -- requests are rejected until the recovery timeout.
    Open,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::HalfOpen => write!(f, "half_open"),
            CircuitState::Open => write!(f, "open"),
        }
    }
}

// ---------------------------------------------------------------------------
// CircuitBreaker
// ---------------------------------------------------------------------------

/// Tracks failure rates and prevents calls to a failing model.
///
/// The state machine follows the classic three-state pattern:
///
/// ```text
/// Closed --(threshold failures)--> Open
/// Open   --(recovery_timeout)----> HalfOpen
/// HalfOpen --(success >= max)----> Closed
/// HalfOpen --(any failure)-------> Open
/// ```
#[derive(Clone)]
pub struct CircuitBreaker {
    state: CircuitState,
    failure_count: usize,
    success_count: usize,
    last_failure_time: Option<Instant>,
    /// Consecutive failures required to trip the circuit open.
    threshold: usize,
    /// How long to wait in the `Open` state before transitioning to `HalfOpen`.
    recovery_timeout: Duration,
    /// Maximum test requests permitted in `HalfOpen` before deciding.
    half_open_max_calls: usize,
    /// Number of test requests already issued in the current `HalfOpen` window.
    half_open_calls: usize,
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("state", &self.state)
            .field("failure_count", &self.failure_count)
            .field("success_count", &self.success_count)
            .field("threshold", &self.threshold)
            .field("recovery_timeout", &self.recovery_timeout)
            .field("half_open_max_calls", &self.half_open_max_calls)
            .finish()
    }
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// * `threshold` -- consecutive failures required to open the circuit
    ///   (defaults to 3 if zero).
    /// * `recovery_timeout` -- time to wait before retrying (defaults to 30 s
    ///   if zero).
    pub fn new(threshold: usize, recovery_timeout: Duration) -> Self {
        let threshold = if threshold == 0 { 3 } else { threshold };
        let recovery_timeout = if recovery_timeout.is_zero() {
            Duration::from_secs(30)
        } else {
            recovery_timeout
        };

        Self {
            state: CircuitState::Closed,
            failure_count: 0,
            success_count: 0,
            last_failure_time: None,
            threshold,
            recovery_timeout,
            half_open_max_calls: 3,
            half_open_calls: 0,
        }
    }

    /// Returns `true` if the circuit allows a request through.
    pub fn allow_request(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if recovery timeout has elapsed.
                let ready = self
                    .last_failure_time
                    .is_some_and(|t| t.elapsed() > self.recovery_timeout);
                if ready {
                    self.state = CircuitState::HalfOpen;
                    self.half_open_calls = 0;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                if self.half_open_calls < self.half_open_max_calls {
                    self.half_open_calls += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful call.
    pub fn record_success(&mut self) {
        match self.state {
            CircuitState::HalfOpen => {
                self.success_count += 1;
                if self.success_count >= self.half_open_max_calls {
                    self.state = CircuitState::Closed;
                    self.failure_count = 0;
                    self.success_count = 0;
                }
            }
            CircuitState::Closed => {
                self.failure_count = 0;
            }
            CircuitState::Open => { /* should not happen, but no-op */ }
        }
    }

    /// Record a failed call.
    pub fn record_failure(&mut self) {
        self.last_failure_time = Some(Instant::now());
        self.failure_count += 1;

        match self.state {
            CircuitState::HalfOpen => {
                self.state = CircuitState::Open;
                self.success_count = 0;
            }
            CircuitState::Closed => {
                if self.failure_count >= self.threshold {
                    self.state = CircuitState::Open;
                }
            }
            CircuitState::Open => { /* already open */ }
        }
    }

    /// Returns the current circuit state.
    pub fn state(&self) -> CircuitState {
        self.state
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(3, Duration::from_secs(30))
    }
}

// ---------------------------------------------------------------------------
// FallbackResult
// ---------------------------------------------------------------------------

/// Outcome of a fallback execution attempt.
#[derive(Debug, Clone)]
pub struct FallbackResult {
    /// The model that was ultimately selected.
    pub model_id: String,
    /// Whether execution succeeded.
    pub success: bool,
    /// Error message (populated on failure).
    pub error: Option<String>,
    /// How many models were tried (1-based).
    pub attempted: usize,
    /// Total number of models in the fallback chain.
    pub total_models: usize,
}

// ---------------------------------------------------------------------------
// FallbackHandler
// ---------------------------------------------------------------------------

/// Manages model fallback with per-model circuit-breaker protection.
pub struct FallbackHandler {
    circuit_breakers: Mutex<HashMap<String, CircuitBreaker>>,
    /// Consecutive failures required to trip a circuit open.
    cb_threshold: usize,
    /// Duration to wait before probing in half-open state.
    cb_timeout: Duration,
}

impl std::fmt::Debug for FallbackHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackHandler")
            .field("cb_threshold", &self.cb_threshold)
            .field("cb_timeout", &self.cb_timeout)
            .finish_non_exhaustive()
    }
}

impl FallbackHandler {
    /// Create a new fallback handler.
    ///
    /// * `cb_threshold` -- failures before opening a circuit (default 3).
    /// * `cb_timeout` -- recovery timeout before half-open probe (default 30 s).
    pub fn new(cb_threshold: usize, cb_timeout: Duration) -> Self {
        let cb_threshold = if cb_threshold == 0 { 3 } else { cb_threshold };
        let cb_timeout = if cb_timeout.is_zero() {
            Duration::from_secs(30)
        } else {
            cb_timeout
        };

        Self {
            circuit_breakers: Mutex::new(HashMap::new()),
            cb_threshold,
            cb_timeout,
        }
    }

    /// Returns (or lazily creates) the circuit breaker for `model_id`.
    fn get_circuit_breaker(&self, model_id: &str) -> CircuitBreaker {
        let mut map = self
            .circuit_breakers
            .lock()
            .expect("circuit breaker lock poisoned");
        map.entry(model_id.to_owned())
            .or_insert_with(|| CircuitBreaker::new(self.cb_threshold, self.cb_timeout))
            .clone()
    }

    /// Replace the circuit breaker for `model_id` (used internally after mutation).
    fn put_circuit_breaker(&self, model_id: &str, cb: CircuitBreaker) {
        let mut map = self
            .circuit_breakers
            .lock()
            .expect("circuit breaker lock poisoned");
        map.insert(model_id.to_owned(), cb);
    }

    /// Try each model in sequence until one succeeds, respecting circuit breakers.
    ///
    /// `executor` is an async closure that receives a reference to the
    /// [`ModelConfig`] and returns the response bytes on success.
    pub async fn execute_with_fallback<E, F>(
        &self,
        models: &[ModelConfig],
        executor: E,
    ) -> (FallbackResult, Option<Vec<u8>>)
    where
        E: Fn(&ModelConfig) -> F,
        F: std::future::Future<Output = Result<Vec<u8>, String>>,
    {
        let total_models = models.len();

        for (i, model) in models.iter().enumerate() {
            let mut cb = self.get_circuit_breaker(&model.model_id);

            // Skip models with open circuit breakers.
            if !cb.allow_request() {
                info!(
                    model = %model.model_id,
                    attempt = i + 1,
                    total = total_models,
                    "circuit breaker open, skipping model"
                );
                self.put_circuit_breaker(&model.model_id, cb);
                continue;
            }

            info!(
                model = %model.model_id,
                attempt = i + 1,
                total = total_models,
                "attempting model"
            );

            // Store updated state (half_open_calls may have been incremented).
            self.put_circuit_breaker(&model.model_id, cb);

            match executor(model).await {
                Ok(body) => {
                    let mut cb = self.get_circuit_breaker(&model.model_id);
                    cb.record_success();
                    self.put_circuit_breaker(&model.model_id, cb);

                    info!(
                        model = %model.model_id,
                        attempt = i + 1,
                        "model succeeded"
                    );

                    return (
                        FallbackResult {
                            model_id: model.model_id.clone(),
                            success: true,
                            error: None,
                            attempted: i + 1,
                            total_models,
                        },
                        Some(body),
                    );
                }
                Err(err) => {
                    let mut cb = self.get_circuit_breaker(&model.model_id);
                    cb.record_failure();
                    let new_state = cb.state();
                    self.put_circuit_breaker(&model.model_id, cb);

                    warn!(
                        model = %model.model_id,
                        error = %err,
                        remaining = total_models - i - 1,
                        circuit_state = %new_state,
                        "model failed, trying fallback"
                    );
                }
            }
        }

        (
            FallbackResult {
                model_id: models
                    .first()
                    .map(|m| m.model_id.clone())
                    .unwrap_or_default(),
                success: false,
                error: Some(format!("all models failed ({total_models} attempts)")),
                attempted: total_models,
                total_models,
            },
            None,
        )
    }

    /// Returns the state of every tracked circuit breaker as model-id to state
    /// string pairs.
    pub fn get_circuit_states(&self) -> HashMap<String, String> {
        let map = self
            .circuit_breakers
            .lock()
            .expect("circuit breaker lock poisoned");
        map.iter()
            .map(|(id, cb)| (id.clone(), cb.state().to_string()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Free helper functions
// ---------------------------------------------------------------------------

/// Returns `true` when the error is worth retrying on a different model.
///
/// Matches against common transient failure signatures: timeouts, connection
/// resets, rate limits (429), and server errors (500 / 502 / 503).
pub fn is_retryable_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    const SIGNALS: &[&str] = &[
        "timeout",
        "connection refused",
        "connection reset",
        "rate limit",
        "429",
        "503",
        "502",
        "500",
    ];
    SIGNALS.iter().any(|sig| lower.contains(sig))
}

/// Build the full fallback chain for `primary` by appending its fallbacks
/// (if any) from the `fallbacks` map.
pub fn get_fallback_chain(
    primary: ModelConfig,
    fallbacks: &HashMap<String, Vec<ModelConfig>>,
) -> Vec<ModelConfig> {
    let mut chain = vec![primary.clone()];

    if let Some(fb) = fallbacks.get(&primary.model_id) {
        chain.extend(fb.iter().cloned());
    }

    chain
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // -- CircuitBreaker state transition tests --------------------------------

    #[test]
    fn new_breaker_starts_closed() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn closed_allows_all_requests() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        assert!(cb.allow_request());
        assert!(cb.allow_request());
    }

    #[test]
    fn threshold_failures_opens_circuit() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed); // 1 failure
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed); // 2 failures
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open); // 3 failures >= threshold
    }

    #[test]
    fn open_circuit_rejects_requests() {
        let mut cb = CircuitBreaker::new(2, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure(); // threshold reached
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn open_transitions_to_half_open_after_timeout() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(10));
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request()); // still within timeout

        // Wait for recovery timeout to elapse.
        std::thread::sleep(Duration::from_millis(20));
        assert!(cb.allow_request()); // should transition to HalfOpen
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_success_closes_circuit() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(10));
        cb.record_failure(); // Open

        std::thread::sleep(Duration::from_millis(20));
        assert!(cb.allow_request()); // -> HalfOpen

        // Need `half_open_max_calls` (default 3) successes to close.
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.failure_count, 0);
    }

    #[test]
    fn half_open_failure_reopens_circuit() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(10));
        cb.record_failure(); // Open
        std::thread::sleep(Duration::from_millis(20));
        assert!(cb.allow_request()); // -> HalfOpen

        cb.record_failure(); // immediate failure -> back to Open
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn half_open_limits_calls() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(10));
        cb.record_failure(); // Open
        std::thread::sleep(Duration::from_millis(20));
        // Open->HalfOpen transition: returns true, sets half_open_calls=0.
        // Then up to half_open_max_calls (3) more calls are allowed.
        // Total allowed: 1 (transition) + 3 (half_open) = 4.
        assert!(cb.allow_request()); // transition to HalfOpen (free call)
        assert!(cb.allow_request()); // half_open call 1
        assert!(cb.allow_request()); // half_open call 2
        assert!(cb.allow_request()); // half_open call 3
        assert!(!cb.allow_request()); // half_open limit reached -> rejected
    }

    #[test]
    fn success_resets_failure_count_when_closed() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.failure_count, 2);
        cb.record_success();
        assert_eq!(cb.failure_count, 0);
        // Should still be closed.
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn default_breaker() {
        let cb = CircuitBreaker::default();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.threshold, 3);
    }

    // -- is_retryable_error tests ---------------------------------------------

    #[test]
    fn retryable_errors() {
        assert!(is_retryable_error("connection timeout exceeded"));
        assert!(is_retryable_error("connection refused by upstream"));
        assert!(is_retryable_error("connection reset by peer"));
        assert!(is_retryable_error("rate limit exceeded"));
        assert!(is_retryable_error("HTTP 429 Too Many Requests"));
        assert!(is_retryable_error("HTTP 503 Service Unavailable"));
        assert!(is_retryable_error("HTTP 502 Bad Gateway"));
        assert!(is_retryable_error("HTTP 500 Internal Server Error"));
    }

    #[test]
    fn non_retryable_errors() {
        assert!(!is_retryable_error("invalid request payload"));
        assert!(!is_retryable_error("model not found"));
        assert!(!is_retryable_error(""));
    }

    #[test]
    fn retryable_error_case_insensitive() {
        assert!(is_retryable_error("TIMEOUT waiting for response"));
        assert!(is_retryable_error("Connection Refused"));
    }

    // -- get_fallback_chain tests ---------------------------------------------

    fn test_model(id: &str) -> ModelConfig {
        ModelConfig {
            provider: "test".into(),
            model_id: id.into(),
            temperature: 0.7,
            max_tokens: 4096,
            context_threshold: 80_000,
            reasoning_effort: "medium".into(),
            thinking: None,
        }
    }

    #[test]
    fn fallback_chain_primary_only() {
        let primary = test_model("glm-5");
        let fallbacks = HashMap::new();
        let chain = get_fallback_chain(primary.clone(), &fallbacks);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].model_id, "glm-5");
    }

    #[test]
    fn fallback_chain_with_fallbacks() {
        let primary = test_model("glm-5");
        let mut fallbacks = HashMap::new();
        fallbacks.insert("glm-5".into(), vec![test_model("kimi"), test_model("qwen")]);
        let chain = get_fallback_chain(primary, &fallbacks);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].model_id, "glm-5");
        assert_eq!(chain[1].model_id, "kimi");
        assert_eq!(chain[2].model_id, "qwen");
    }

    // -- FallbackHandler tests ------------------------------------------------

    #[tokio::test]
    async fn fallback_handler_first_model_succeeds() {
        let handler = FallbackHandler::new(3, Duration::from_secs(30));
        let models = vec![test_model("glm-5"), test_model("kimi")];

        let (result, body) = handler
            .execute_with_fallback(&models, |_m| async { Ok::<_, String>(vec![1, 2, 3]) })
            .await;

        assert!(result.success);
        assert_eq!(result.model_id, "glm-5");
        assert_eq!(result.attempted, 1);
        assert_eq!(body.unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn fallback_handler_falls_through_on_error() {
        let handler = FallbackHandler::new(3, Duration::from_secs(30));
        let models = vec![test_model("glm-5"), test_model("kimi")];

        let call_count = AtomicUsize::new(0);
        let (result, body) = handler
            .execute_with_fallback(&models, |m| {
                let count = call_count.fetch_add(1, Ordering::SeqCst);
                let model_id = m.model_id.clone();
                async move {
                    if model_id == "glm-5" {
                        Err::<Vec<u8>, _>("timeout".into())
                    } else {
                        assert_eq!(count, 1);
                        Ok(vec![42])
                    }
                }
            })
            .await;

        assert!(result.success);
        assert_eq!(result.model_id, "kimi");
        assert_eq!(result.attempted, 2);
        assert_eq!(body.unwrap(), vec![42]);
    }

    #[tokio::test]
    async fn fallback_handler_all_models_fail() {
        let handler = FallbackHandler::new(3, Duration::from_secs(30));
        let models = vec![test_model("glm-5"), test_model("kimi")];

        let (result, body) = handler
            .execute_with_fallback(&models, |_m| async {
                Err::<Vec<u8>, _>("500 error".into())
            })
            .await;

        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(body.is_none());
        assert_eq!(result.attempted, 2);
    }

    #[tokio::test]
    async fn fallback_handler_skips_open_circuit() {
        let handler = FallbackHandler::new(1, Duration::from_secs(300));
        let models = vec![test_model("glm-5"), test_model("kimi")];

        // Trip the circuit for glm-5.
        {
            let mut map = handler.circuit_breakers.lock().unwrap();
            let mut cb = CircuitBreaker::new(1, Duration::from_secs(300));
            cb.record_failure(); // 1 >= threshold -> Open
            assert_eq!(cb.state(), CircuitState::Open);
            map.insert("glm-5".into(), cb);
        }

        let (result, body) = handler
            .execute_with_fallback(&models, |m| {
                let id = m.model_id.clone();
                async move { Ok::<_, String>(id.as_bytes().to_vec()) }
            })
            .await;

        // glm-5 skipped due to open circuit; kimi succeeds.
        assert!(result.success);
        assert_eq!(result.model_id, "kimi");
        assert_eq!(body.unwrap(), b"kimi".to_vec());
    }

    #[test]
    fn fallback_handler_circuit_states() {
        let handler = FallbackHandler::new(1, Duration::from_secs(30));

        // No breakers yet.
        assert!(handler.get_circuit_states().is_empty());

        // Force an open breaker for one model.
        {
            let mut map = handler.circuit_breakers.lock().unwrap();
            let mut cb = CircuitBreaker::new(1, Duration::from_secs(30));
            cb.record_failure();
            map.insert("glm-5".into(), cb);
        }

        let states = handler.get_circuit_states();
        assert_eq!(states.get("glm-5").unwrap(), "open");
    }
}
