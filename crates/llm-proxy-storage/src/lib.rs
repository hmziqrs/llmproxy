//! Structured request/response event log surface.
//!
//! The persistence backend (SQLite, etc.) lands in a later phase. This crate
//! defines the event types and the [`EventBus`] trait that backends implement,
//! plus a [`NoopBus`] default and a [`RecordingBus`] for tests.
//!
//! Events are emitted at request/response boundaries in the server crate. A
//! real persisted backend will implement [`EventBus`]; for now [`NoopBus`] is
//! the production sink and [`RecordingBus`] (behind the `test-utils` cargo
//! feature) lets integration tests assert on the event sequence.

use llm_proxy_protocol::core::{Cost, ModelRef, StopReason, Usage};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ---------------------------------------------------------------------------
// ProxyEvent
// ---------------------------------------------------------------------------

/// One request/response lifecycle event, ready to persist or emit.
///
/// Internally tagged via `kind` so JSON/TOML consumers can discriminate without
/// a wrapper object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyEvent {
    /// A request was received and routed.
    RequestReceived(RequestReceived),
    /// A response completed successfully.
    ResponseCompleted(ResponseCompleted),
    /// A response failed before or during upstream dispatch.
    ResponseFailed(ResponseFailed),
}

// ---------------------------------------------------------------------------
// RequestReceived
// ---------------------------------------------------------------------------

/// Event payload for a received request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestReceived {
    /// Proxy-generated unique request identifier.
    pub request_id: String,
    /// When the request was received (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Provider name from the URL path.
    pub provider: String,
    /// Route kind (`"chat_completions"` or `"messages"`).
    pub route_kind: String,
    /// Client protocol (`"openai_chat"` or `"anthropic"`).
    pub client_protocol: String,
    /// Requested and upstream model.
    pub model: ModelRef,
    /// Whether the request requested streaming.
    pub streaming: bool,
    /// SHA-256 of the raw client request body for dedup/correlation (not the
    /// body itself, which may contain secrets).
    pub body_hash: String,
}

// ---------------------------------------------------------------------------
// ResponseCompleted
// ---------------------------------------------------------------------------

/// Event payload for a completed response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseCompleted {
    /// Proxy-generated unique request identifier.
    pub request_id: String,
    /// When the response completed (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Provider name.
    pub provider: String,
    /// Upstream message id, if available from `MessageStart` or
    /// `CoreResponse.id`.
    pub upstream_message_id: Option<String>,
    /// Requested and upstream model.
    pub model: ModelRef,
    /// Token usage reported by the provider.
    pub usage: Usage,
    /// Computed cost, if pricing was configured for the model.
    pub cost: Option<Cost>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// Request latency in milliseconds.
    pub latency_ms: u64,
}

// ---------------------------------------------------------------------------
// ResponseFailed
// ---------------------------------------------------------------------------

/// Event payload for a failed response.
///
/// `model` and `provider` are optional because early failures (unknown
/// provider, JSON parse error, rate limit) can occur before the model or
/// provider is confirmed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFailed {
    /// Proxy-generated unique request identifier.
    pub request_id: String,
    /// When the failure occurred (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Provider name, if known at failure time.
    pub provider: Option<String>,
    /// Requested and upstream model, if decoded before failure.
    pub model: Option<ModelRef>,
    /// Error kind string from the `RouteError` variant name.
    pub error_kind: String,
    /// Sanitized error message; never includes api keys or raw upstream bodies.
    pub message: String,
    /// HTTP status code returned to the client.
    pub http_status: u16,
    /// Request latency in milliseconds.
    pub latency_ms: u64,
}

// ---------------------------------------------------------------------------
// EventBus trait + implementations
// ---------------------------------------------------------------------------

/// In-process event sink.
///
/// Implementations: [`NoopBus`], [`RecordingBus`] (tests), and the future
/// SQLite backend.
///
/// `Debug` is a super-trait so that `Arc<dyn EventBus>` can be formatted in
/// `AppState`'s manual `Debug` impl.
pub trait EventBus: Send + Sync + std::fmt::Debug {
    /// Emit one event. Must not block the calling task; backends do IO on a
    /// dedicated writer task.
    fn emit(&self, event: &ProxyEvent);
}

/// Default no-op sink.
#[derive(Debug, Clone, Default)]
pub struct NoopBus;

impl EventBus for NoopBus {
    fn emit(&self, _event: &ProxyEvent) {}
}

/// Test-only sink that records events for assertions. Available behind the
/// `test-utils` cargo feature (NOT `#[cfg(test)]`, which is stripped when the
/// crate is compiled as a dependency).
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Default)]
pub struct RecordingBus {
    /// Recorded events, guarded by a mutex for interior mutability.
    pub events: std::sync::Mutex<Vec<ProxyEvent>>,
}

#[cfg(any(test, feature = "test-utils"))]
impl RecordingBus {
    /// Create an empty recording bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the recorded events (cloned under the lock).
    pub fn snapshot(&self) -> Vec<ProxyEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl EventBus for RecordingBus {
    fn emit(&self, event: &ProxyEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_proxy_protocol::core::{Usage, UsageProvenance};

    fn sample_request_received() -> RequestReceived {
        RequestReceived {
            request_id: "req-1".to_owned(),
            timestamp: OffsetDateTime::now_utc(),
            provider: "p".to_owned(),
            route_kind: "messages".to_owned(),
            client_protocol: "anthropic".to_owned(),
            model: ModelRef {
                requested: "claude".to_owned(),
                upstream: None,
            },
            streaming: false,
            body_hash: "abc123".to_owned(),
        }
    }

    #[test]
    fn noop_bus_emit_does_not_panic_or_record() {
        let bus = NoopBus;
        // NoopBus has nowhere to record; just ensure emit is callable.
        let event = ProxyEvent::RequestReceived(sample_request_received());
        bus.emit(&event);
    }

    #[test]
    fn recording_bus_captures_events_in_order() {
        let bus = RecordingBus::new();
        let req = ProxyEvent::RequestReceived(sample_request_received());
        let done = ProxyEvent::ResponseCompleted(ResponseCompleted {
            request_id: "req-1".to_owned(),
            timestamp: OffsetDateTime::now_utc(),
            provider: "p".to_owned(),
            upstream_message_id: Some("msg_42".to_owned()),
            model: ModelRef {
                requested: "claude".to_owned(),
                upstream: None,
            },
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
            cost: None,
            stop_reason: StopReason::EndTurn,
            latency_ms: 123,
        });
        bus.emit(&req);
        bus.emit(&done);

        let events = bus.snapshot();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ProxyEvent::RequestReceived(_)));
        assert!(matches!(events[1], ProxyEvent::ResponseCompleted(_)));
    }

    #[test]
    fn proxy_event_serializes_with_kind_tag_and_rfc3339_timestamp() {
        let event = ProxyEvent::RequestReceived(sample_request_received());
        let json = serde_json::to_string(&event).expect("serialize");
        // Internally tagged by `kind`.
        assert!(json.contains(r#""kind":"request_received""#), "got: {json}");
        // Timestamp is RFC 3339 (contains a 'T' and a timezone offset/Z).
        assert!(json.contains('T'), "timestamp not rfc3339: {json}");
    }

    #[test]
    fn response_failed_optional_fields_round_trip() {
        let event = ProxyEvent::ResponseFailed(ResponseFailed {
            request_id: "req-2".to_owned(),
            timestamp: OffsetDateTime::now_utc(),
            provider: None,
            model: None,
            error_kind: "UnknownProvider".to_owned(),
            message: "no such provider".to_owned(),
            http_status: 404,
            latency_ms: 1,
        });
        let json = serde_json::to_string(&event).expect("serialize");
        let back: ProxyEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, ProxyEvent::ResponseFailed(_)));
        // Sanity: UsageProvenance must be referenced so it is not flagged unused.
        let _ = UsageProvenance::Unknown;
    }
}
