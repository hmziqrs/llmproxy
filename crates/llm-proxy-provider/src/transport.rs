//! Protocol-neutral HTTP transport for upstream LLM providers.
//!
//! This module provides a thin transport layer that sends prepared bytes to a
//! prepared URL with prepared auth headers. It knows nothing about protocol
//! names, model IDs, or provider adapters.
//!
//! ## Neutrality guardrails
//!
//! The transport layer must not import or mention endpoint classification,
//! provider model names, or protocol crate types. It only sends prepared
//! bytes to a prepared URL with prepared auth headers.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use futures::TryStreamExt;
use llm_proxy_core::AuthStyle;
use tokio::time::Sleep;

use crate::error::ProviderError;

// ---------------------------------------------------------------------------
// AuthHeaders
// ---------------------------------------------------------------------------

/// Authentication headers for an upstream request.
///
/// The `api_key` field is redacted in [`fmt::Debug`] output so that
/// `tracing::debug!(?auth)` or snapshot output never leaks the secret.
#[derive(Clone)]
#[non_exhaustive]
pub struct AuthHeaders {
    /// Which header style to use for the API key.
    pub style: AuthStyle,
    /// The API key value. Redacted in Debug output.
    pub api_key: String,
}

impl fmt::Debug for AuthHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthHeaders")
            .field("style", &self.style)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ProxyRequest
// ---------------------------------------------------------------------------

/// A protocol-neutral request to send to an upstream provider.
///
/// The transport sends `body` byte-for-byte. It does not serialize typed
/// request structs.
///
/// The `auth.api_key` field is redacted in [`fmt::Debug`] output.
#[derive(Clone)]
#[non_exhaustive]
pub struct ProxyRequest {
    /// Full upstream URL.
    pub url: String,
    /// Authentication headers.
    pub auth: AuthHeaders,
    /// Raw request body bytes.
    pub body: Vec<u8>,
    /// Whether this is a streaming request (informational for logging/metrics).
    ///
    /// **This field is NOT read by [`ProxyClient`].** The caller must pick the
    /// correct method: [`ProxyClient::send`] for non-streaming requests and
    /// [`ProxyClient::send_stream`] for streaming requests. This field exists
    /// solely for structured logging and future metrics.
    ///
    /// LOW-1 (evaluated, deferred by design): the audit's softened analysis
    /// confirms the two production call sites (`core_pipeline.rs`) pick
    /// `send`/`send_stream` statically from routing — there is no runtime
    /// `if req.stream` dispatch, so the field cannot cause a wrong-method call.
    /// Promoting the documented invariant to a compile-time one would require
    /// type-state (`ProxyRequest<Streaming>` / `<NonStreaming>`) or an enum
    /// return, but `core.stream` is a *runtime* `bool`, so pure type-state is
    /// infeasible without splitting the encode path too. Rated low-impact /
    /// high-churn by the audit itself; the documentation invariant is retained.
    pub stream: bool,
    /// Static adapter headers from config (e.g. `anthropic-version`).
    ///
    /// These are validated at config-load time and applied to every upstream
    /// request in addition to auth headers.
    pub extra_headers: std::collections::HashMap<String, String>,
}

impl fmt::Debug for ProxyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyRequest")
            .field("url", &endpoint_without_query(&self.url))
            .field("auth", &self.auth)
            .field("body_len", &self.body.len())
            .field("stream", &self.stream)
            .finish()
    }
}

pub(crate) fn endpoint_without_query(endpoint: &str) -> &str {
    endpoint
        .split_once('?')
        .map_or(endpoint, |(base, _query)| base)
}

// ---------------------------------------------------------------------------
// ProxyClient
// ---------------------------------------------------------------------------

/// Default per-chunk idle read timeout for streaming upstream responses.
///
/// Legitimate SSE providers send heartbeats / keep-alive bytes (e.g.
/// Anthropic's `event: ping`, OpenAI's immediate first chunk) well within this
/// window, so a stream that yields no byte for longer than this is a dead or
/// wedged upstream. A stalled provider can no longer hold the connection open
/// indefinitely (audit GAP-MED-2).
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Protocol-neutral HTTP client for upstream LLM providers.
///
/// Owns a connection-pooled [`reqwest::Client`] and sends [`ProxyRequest`]
/// instances without any knowledge of protocol names or model IDs.
#[derive(Debug, Clone)]
#[must_use = "ProxyClient does nothing until send/send_stream is called"]
pub struct ProxyClient {
    http: reqwest::Client,
    /// Per-chunk idle read timeout applied to streaming responses. If the
    /// upstream yields no body byte within this window the stream errors so a
    /// stalled provider cannot hold the connection open (audit GAP-MED-2).
    stream_idle_timeout: Duration,
}

impl Default for ProxyClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyClient {
    /// Creates a new transport client with sensible connection pool defaults.
    ///
    /// This is a convenience wrapper around [`Self::try_new`] that unwraps the
    /// result. The current configuration (no custom TLS backend, no proxy env
    /// validation at build time) makes `reqwest::Client::build()` infallible
    /// in practice. If a future configuration change makes this fallible,
    /// callers should switch to [`Self::try_new`].
    ///
    /// # Lint suppression (audit LOW-2)
    ///
    /// The `expect` below is deliberate: this is the documented infallible-in-
    /// practice convenience constructor, with [`Self::try_new`] offered as the
    /// fallible alternative. The `#[allow(clippy::expect_used)]` annotation is
    /// intentionally retained even though `clippy::expect_used` (a
    /// `clippy::restriction` lint, not part of `clippy::all`) is currently
    /// disabled in `[workspace.lints.clippy]` and so suppresses nothing today.
    /// It is a defensive guard: if the workspace later enables the restriction
    /// group, this intentional `.expect` site stays acknowledged instead of
    /// surfacing as a fresh diagnostic. Converting to `#[expect(...)]` would
    /// *activate* the lint here and emit the real "used `expect` on `Ok`
    /// value" warning (per the LOW-2 verifier note), which is not desired.
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        Self::try_new().expect("failed to build reqwest client with default configuration")
    }

    /// Creates a new transport client, returning an error if the HTTP client
    /// cannot be built.
    ///
    /// Use this when configuration may make client construction fallible
    /// (e.g. custom TLS backends, proxy environment validation).
    pub fn try_new() -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self {
            http,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
        })
    }

    /// Override the per-chunk idle read timeout applied to streaming responses.
    ///
    /// Returns the client for chaining. The default is
    /// [`DEFAULT_STREAM_IDLE_TIMEOUT`] (60 s); legitimate SSE providers
    /// heartbeat well within that window. Raise it only if a specific upstream
    /// is known to pause longer between bytes (audit GAP-MED-2).
    pub fn with_stream_idle_timeout(mut self, idle: Duration) -> Self {
        self.stream_idle_timeout = idle;
        self
    }

    /// Sends a non-streaming request and returns the response body bytes.
    ///
    /// - Always sets `Content-Type: application/json`.
    /// - Does **not** set `Accept: text/event-stream`.
    /// - Returns [`ProviderError::Api`] for HTTP status >= 400.
    pub async fn send(&self, req: ProxyRequest) -> Result<Vec<u8>, ProviderError> {
        let mut builder = self
            .http
            .post(&req.url)
            .header("Content-Type", "application/json");

        builder = apply_auth(builder, &req.auth);

        for (name, value) in &req.extra_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }

        let resp = builder.body(req.body).send().await?;

        check_status(resp).await
    }

    /// Sends a streaming request and returns a byte stream.
    ///
    /// # Cancel safety
    ///
    /// The returned stream owns the `reqwest::Response` via `bytes_stream()`.
    /// Dropping the stream drops the underlying connection, aborting the
    /// upstream request. No detached task is spawned that outlives the
    /// consumer.
    ///
    /// - Always sets `Content-Type: application/json`.
    /// - Sets `Accept: text/event-stream`.
    /// - Returns [`ProviderError::Api`] for HTTP status >= 400 **before**
    ///   any byte stream is exposed.
    /// - Stream item errors are wrapped as [`ProviderError`]; consumers never
    ///   see raw `reqwest::Error`.
    /// - Dropping the returned stream aborts the in-flight upstream request.
    pub async fn send_stream(
        &self,
        req: ProxyRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send + 'static>>,
        ProviderError,
    > {
        let mut builder = self
            .http
            .post(&req.url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");

        builder = apply_auth(builder, &req.auth);

        for (name, value) in &req.extra_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }

        let resp = builder.body(req.body).send().await?;

        if resp.status().as_u16() >= 400 {
            let status = resp.status().as_u16();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read error body: {}>", e));
            return Err(ProviderError::api(status, body_text));
        }

        // `bytes_stream()` consumes the Response and returns an owned stream;
        // dropping the stream drops the underlying connection, aborting the
        // upstream request. Do not spawn a detached task that outlives the
        // consumer.
        let stream = resp.bytes_stream();
        let mapped = stream.map_err(ProviderError::from);
        // Wrap with a per-chunk idle read timeout so a stalled upstream (one
        // that opens the connection but then sends no bytes) cannot hold the
        // proxy's connection open indefinitely (audit GAP-MED-2).
        let inner: Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>> =
            Box::pin(mapped);
        Ok(Box::pin(IdleTimeoutStream::new(
            inner,
            self.stream_idle_timeout,
        )))
    }
}

// ---------------------------------------------------------------------------
// IdleTimeoutStream
// ---------------------------------------------------------------------------

/// A byte-stream wrapper enforcing a per-chunk idle read timeout.
///
/// If the inner stream does not yield an item within `idle` of the previous one
/// (or of subscription, for the first chunk), the next poll returns
/// [`ProviderError::Http`] with `timeout: true` so the consumer can close the
/// connection. This prevents a stalled upstream — one that opens the
/// connection then sends no bytes — from holding the proxy's connection open
/// forever (audit GAP-MED-2).
///
/// An *idle* (not *overall*) timeout is used deliberately: a legitimate long
/// LLM token stream can run for minutes and must not be killed merely for
/// being long, only for going silent. Legitimate SSE providers heartbeat
/// within the idle window.
struct IdleTimeoutStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>,
    /// Timer armed for `idle` after the most recent chunk (or at construction).
    /// `Sleep` is `!Unpin`, so it is boxed; `Pin<Box<_>>` is `Unpin`, keeping
    /// the whole wrapper `Unpin` and the manual `Stream` impl safe.
    sleep: Pin<Box<Sleep>>,
    idle: Duration,
}

impl IdleTimeoutStream {
    fn new(
        inner: Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>,
        idle: Duration,
    ) -> Self {
        Self {
            inner,
            sleep: Box::pin(tokio::time::sleep(idle)),
            idle,
        }
    }
}

impl Stream for IdleTimeoutStream {
    type Item = Result<Bytes, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Every field is a `Pin<Box<_>>` or `Duration` (all `Unpin`), so the
        // wrapper itself is `Unpin` and `Pin<&mut Self>::get_mut` is sound.
        let this = self.get_mut();

        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                // Restart the idle window for the next chunk.
                this.sleep = Box::pin(tokio::time::sleep(this.idle));
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                // No chunk yet — is the idle window elapsed?
                match this.sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Some(Err(ProviderError::Http {
                        message: format!(
                            "upstream stream stalled: no data within {}s \
                             (stream idle timeout); aborting to avoid an \
                             open-ended stall (audit GAP-MED-2)",
                            this.idle.as_secs()
                        ),
                        timeout: true,
                    }))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Apply authentication headers to a request builder based on [`AuthStyle`].
///
/// # Security note on `AuthStyle::Both`
///
/// When `AuthStyle::Both` is used, the API key is sent in **both** the
/// `Authorization: Bearer` and `x-api-key` headers simultaneously. This
/// doubles the exposure surface of the secret in the wire data. This mode
/// exists for compatibility with providers that accept either header (e.g.,
/// Anthropic). Where possible, prefer `AuthStyle::Bearer` or
/// `AuthStyle::XApiKey` to send the key in only one header.
fn apply_auth(mut builder: reqwest::RequestBuilder, auth: &AuthHeaders) -> reqwest::RequestBuilder {
    match auth.style {
        AuthStyle::Bearer => {
            builder = builder.header("Authorization", format!("Bearer {}", auth.api_key));
        }
        AuthStyle::XApiKey => {
            builder = builder.header("x-api-key", &auth.api_key);
        }
        AuthStyle::XGoogleApiKey => {
            builder = builder.header("x-goog-api-key", &auth.api_key);
        }
        AuthStyle::Both => {
            builder = builder.header("Authorization", format!("Bearer {}", auth.api_key));
            builder = builder.header("x-api-key", &auth.api_key);
        }
    }
    builder
}

/// Check response status and return body bytes or a [`ProviderError::Api`].
async fn check_status(resp: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    if resp.status().as_u16() >= 400 {
        let status = resp.status().as_u16();
        let body_text = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read error body: {}>", e));
        return Err(ProviderError::api(status, body_text));
    }
    let body = resp.bytes().await?;
    Ok(body.to_vec())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Debug redaction tests
    // -----------------------------------------------------------------------

    #[test]
    fn auth_headers_debug_redacts_api_key() {
        let auth = AuthHeaders {
            style: AuthStyle::Bearer,
            api_key: "sk-super-secret-key-12345".to_owned(),
        };
        let debug_output = format!("{:?}", auth);
        assert!(
            !debug_output.contains("sk-super-secret-key-12345"),
            "Debug output must not contain the api_key: {}",
            debug_output
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must contain redacted marker: {}",
            debug_output
        );
    }

    #[test]
    fn proxy_request_debug_redacts_api_key() {
        let req = ProxyRequest {
            url: "https://api.example.com/v1/chat/completions?key=query-secret".to_owned(),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "sk-super-secret-key-12345".to_owned(),
            },
            body: br#"{"model":"gpt-5"}"#.to_vec(),
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };
        let debug_output = format!("{:?}", req);
        assert!(
            !debug_output.contains("sk-super-secret-key-12345"),
            "Debug output must not contain the api_key: {}",
            debug_output
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must contain redacted marker: {}",
            debug_output
        );
        assert!(!debug_output.contains("query-secret"));
    }

    // -----------------------------------------------------------------------
    // Transport integration tests (axum test server)
    // -----------------------------------------------------------------------

    use axum::Router;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use futures::StreamExt;
    use tokio::net::TcpListener;

    /// A simple axum handler that echoes back the request body and headers.
    async fn echo_handler(headers: HeaderMap, body: bytes::Bytes) -> impl IntoResponse {
        let mut response_parts = Vec::new();

        // Echo all headers as "name: value" pairs.
        for (name, value) in &headers {
            response_parts.push(format!(
                "{}: {}",
                name.as_str(),
                value.to_str().unwrap_or("?")
            ));
        }

        // Echo body
        let body_str = String::from_utf8_lossy(&body);
        response_parts.push(format!("body: {}", body_str));

        (StatusCode::OK, response_parts.join("\n"))
    }

    /// Handler that echoes request headers as a streaming SSE response.
    async fn echo_stream_handler(headers: HeaderMap, body: bytes::Bytes) -> impl IntoResponse {
        let mut events = Vec::new();

        // Echo Content-Type
        if let Some(ct) = headers.get("content-type") {
            events.push(format!(
                "data: {{\"content-type\": \"{}\"}}\n\n",
                ct.to_str().unwrap_or("?")
            ));
        }

        // Echo Authorization
        if let Some(auth) = headers.get("authorization") {
            events.push(format!(
                "data: {{\"authorization\": \"{}\"}}\n\n",
                auth.to_str().unwrap_or("?")
            ));
        }

        // Echo x-api-key
        if let Some(key) = headers.get("x-api-key") {
            events.push(format!(
                "data: {{\"x-api-key\": \"{}\"}}\n\n",
                key.to_str().unwrap_or("?")
            ));
        }

        // Echo Accept
        if let Some(accept) = headers.get("accept") {
            events.push(format!(
                "data: {{\"accept\": \"{}\"}}\n\n",
                accept.to_str().unwrap_or("?")
            ));
        }

        // Echo body
        let body_str = String::from_utf8_lossy(&body);
        events.push(format!("data: {{\"body\": \"{}\"}}\n\n", body_str));

        // Terminal frame
        events.push("data: [DONE]\n\n".to_owned());

        let stream = futures::stream::iter(events);
        let body_stream = stream.map(|s| Ok::<_, std::convert::Infallible>(bytes::Bytes::from(s)));

        (
            StatusCode::OK,
            [("Content-Type", "text/event-stream")],
            Body::from_stream(body_stream),
        )
    }

    /// Handler that returns a 400 error with a JSON body.
    async fn error_handler(State(code): State<u16>) -> impl IntoResponse {
        let body = format!(r#"{{"error": "upstream error", "status": {}}}"#, code);
        (
            StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
            body,
        )
    }

    /// Wait for a test server to be ready by polling its TCP port.
    ///
    /// Replaces the previous fixed `tokio::time::sleep(Duration::from_millis(50))`
    /// with a deterministic readiness check that retries until the server
    /// accepts a connection or the deadline elapses (audit LOW-19). This is the
    /// same pattern used in `tests/chat_completions.rs` and `tests/core_pipeline.rs`.
    async fn wait_for_ready(addr: std::net::SocketAddr) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("test server at {addr} did not become ready within 5 s");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Spin up an ephemeral axum server, return its base URL once it is ready.
    async fn start_test_server(routes: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });

        // Wait deterministically for the server to accept connections instead
        // of a fixed sleep (audit LOW-19).
        wait_for_ready(addr).await;

        base
    }

    #[tokio::test]
    async fn bearer_header_set() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "test-key-123".to_owned(),
            },
            body: br#"{"hello":"world"}"#.to_vec(),
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("authorization: Bearer test-key-123"),
            "Response must contain Bearer header: {}",
            text
        );
        assert!(
            !text.contains("x-api-key"),
            "Bearer auth style must NOT set x-api-key header: {}",
            text
        );
    }

    #[tokio::test]
    async fn x_api_key_header_set() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::XApiKey,
                api_key: "test-key-456".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("x-api-key: test-key-456"),
            "Response must contain x-api-key header: {}",
            text
        );
        assert!(
            !text.contains("authorization"),
            "XApiKey auth style must NOT set Authorization header: {}",
            text
        );
    }

    #[tokio::test]
    async fn both_headers_set() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Both,
                api_key: "test-key-789".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("authorization: Bearer test-key-789"),
            "Response must contain Bearer header: {}",
            text
        );
        assert!(
            text.contains("x-api-key: test-key-789"),
            "Response must contain x-api-key header: {}",
            text
        );
    }

    #[tokio::test]
    async fn x_google_api_key_sets_correct_header() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::XGoogleApiKey,
                api_key: "google-test-key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("x-goog-api-key: google-test-key"),
            "Response must contain x-goog-api-key header: {}",
            text
        );
        // Must NOT contain Bearer or x-api-key.
        assert!(
            !text.contains("authorization:"),
            "XGoogleApiKey must not set Authorization header: {}",
            text
        );
        assert!(
            !text.contains("x-api-key:"),
            "XGoogleApiKey must not set x-api-key header: {}",
            text
        );
    }

    #[tokio::test]
    async fn extra_headers_are_sent_in_non_streaming_request() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let mut extra = std::collections::HashMap::new();
        extra.insert("anthropic-version".to_owned(), "2023-06-01".to_owned());
        extra.insert("x-custom".to_owned(), "custom-value".to_owned());

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: extra,
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("anthropic-version: 2023-06-01"),
            "Response must contain static adapter header: {}",
            text
        );
        assert!(
            text.contains("x-custom: custom-value"),
            "Response must contain static adapter header: {}",
            text
        );
    }

    #[tokio::test]
    async fn extra_headers_are_sent_in_streaming_request() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let mut extra = std::collections::HashMap::new();
        extra.insert("x-stream-header".to_owned(), "stream-value".to_owned());

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: br#"data: hello"#.to_vec(),
            stream: true,
            extra_headers: extra,
        };

        // The streaming request should succeed (status < 400) because the
        // extra headers are applied before the body is sent.
        let result = client.send_stream(req).await;
        assert!(
            result.is_ok(),
            "streaming request with extra headers should succeed"
        );
    }

    #[tokio::test]
    async fn content_type_always_set_to_application_json() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: br#"{"hello":"world"}"#.to_vec(),
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("content-type: application/json"),
            "Content-Type must always be application/json: {}",
            text
        );
    }

    #[tokio::test]
    async fn non_json_body_sent_byte_for_byte() {
        // Verify that arbitrary non-JSON bytes pass through unchanged.
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let raw_body: Vec<u8> = vec![0x00, 0x01, 0x02, 0xFF, 0xFE];
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: raw_body.clone(),
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let resp_bytes = resp;
        // The echo handler uses String::from_utf8_lossy, so check that the
        // lossy-converted body contains the expected bytes.
        let text = String::from_utf8_lossy(&resp_bytes);
        // The raw bytes 0x00, 0x01, 0x02 will appear literally; 0xFF, 0xFE
        // become the replacement char. Verify the body was received.
        assert!(
            text.contains("body:"),
            "Response must echo body field: {}",
            text
        );
        // Verify the exact raw bytes were received by checking the first few.
        let body_prefix = b"body: \x00\x01\x02";
        assert!(
            resp_bytes
                .windows(body_prefix.len())
                .any(|w| w == body_prefix),
            "Response must contain exact raw bytes: {:?}",
            resp_bytes
        );
    }

    #[tokio::test]
    async fn error_status_returns_provider_error_api() {
        let app = Router::new()
            .route("/test", post(error_handler))
            .with_state(429u16);
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let err = client.send(req).await.unwrap_err();
        match err {
            ProviderError::Api { status, body } => {
                assert_eq!(status, 429);
                assert!(body.contains("upstream error"));
            }
            other => panic!("expected ProviderError::Api, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn streaming_error_status_returns_provider_error_api() {
        let app = Router::new()
            .route("/test", post(error_handler))
            .with_state(500u16);
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: true,
            extra_headers: std::collections::HashMap::new(),
        };

        let result = client.send_stream(req).await;
        assert!(result.is_err(), "expected error for 500 status");
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected ProviderError::Api"),
        };
        match err {
            ProviderError::Api { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("upstream error"));
            }
            other => panic!("expected ProviderError::Api, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn stream_request_sets_accept_event_stream() {
        let app = Router::new().route("/test", post(echo_stream_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: true,
            extra_headers: std::collections::HashMap::new(),
        };

        let mut stream = client.send_stream(req).await.unwrap();

        // Collect all events and look for the Accept header echo.
        let mut found_accept = false;
        while let Some(item) = stream.next().await {
            let chunk = item.unwrap();
            let text = String::from_utf8_lossy(&chunk);
            if text.contains("text/event-stream") {
                found_accept = true;
            }
        }
        assert!(
            found_accept,
            "Stream response must show Accept: text/event-stream was set"
        );
    }

    #[tokio::test]
    async fn non_stream_request_does_not_set_accept_event_stream() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            !text.contains("text/event-stream"),
            "Non-stream request must NOT set Accept: text/event-stream: {}",
            text
        );
    }

    #[tokio::test]
    async fn request_body_sent_byte_for_byte() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let original_body = br#"{"model":"gpt-5","messages":[{"role":"user","content":"hi"}]}"#;
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: original_body.to_vec(),
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        let expected = std::str::from_utf8(original_body).unwrap();
        assert!(
            text.contains(expected),
            "Response must echo the exact request body: {}",
            text
        );
    }

    #[tokio::test]
    async fn dropping_stream_aborts_upstream() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        // A proper handler function that sends data slowly so we can drop mid-stream.
        async fn slow_handler(
            axum::extract::State(counter): axum::extract::State<Arc<AtomicUsize>>,
        ) -> axum::response::Response {
            let stream = futures::stream::unfold(0u32, move |i| {
                let counter = counter.clone();
                async move {
                    if i >= 100 {
                        None
                    } else {
                        counter.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        let chunk = format!("data: chunk {}\n\n", i);
                        Some((
                            Ok::<_, std::convert::Infallible>(bytes::Bytes::from(chunk)),
                            i + 1,
                        ))
                    }
                }
            });
            (
                StatusCode::OK,
                [("Content-Type", "text/event-stream")],
                Body::from_stream(stream),
            )
                .into_response()
        }

        let app = Router::new()
            .route("/test", post(slow_handler))
            .with_state(counter_clone);
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: true,
            extra_headers: std::collections::HashMap::new(),
        };

        {
            let mut stream = client.send_stream(req).await.unwrap();
            // Read one item to confirm stream is working.
            let _first = stream.next().await;
            // Stream is dropped here when it goes out of scope.
        }

        // Poll the counter until it stabilizes (two consecutive identical
        // samples) so the assertion sees the post-abort steady state instead
        // of racing the handler's last in-flight chunk. Replaces the previous
        // fixed 200 ms sleep + timing-dependent `count < 50` assertion with a
        // deterministic wait and a deterministic property (audit LOW-19).
        let mut count = counter.load(Ordering::SeqCst);
        let poll_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let next = counter.load(Ordering::SeqCst);
            if next == count {
                break;
            }
            count = next;
            if tokio::time::Instant::now() >= poll_deadline {
                // Still incrementing after 5 s — the abort did not propagate;
                // fall through to the assertion which will then fail loudly.
                break;
            }
        }

        // The deterministic property: aborting the stream must stop the
        // handler short of its 100-chunk limit. The handler stops emitting at
        // `i >= 100`, so reaching 100 means the stream ran to completion and
        // the drop did NOT abort upstream. Any value < 100 proves the abort
        // took effect (the exact count depends on scheduling and is not
        // itself the property under test).
        assert!(
            count < 100,
            "Dropping the stream should abort upstream before it runs to \
             completion; counter was {}",
            count
        );
    }

    /// Handler that yields one chunk then stalls (sends no further byte and
    /// keeps the connection open) to exercise the per-chunk idle timeout.
    async fn stalling_handler() -> axum::response::Response {
        let stream = futures::stream::unfold(false, |sent| async move {
            if !sent {
                Some((
                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from("data: first\n\n")),
                    true,
                ))
            } else {
                // Stall far longer than any reasonable idle timeout.
                tokio::time::sleep(Duration::from_secs(120)).await;
                None
            }
        });
        (
            StatusCode::OK,
            [("Content-Type", "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response()
    }

    #[tokio::test]
    async fn stalled_upstream_triggers_idle_timeout() {
        let app = Router::new().route("/test", post(stalling_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new().with_stream_idle_timeout(Duration::from_millis(100));
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: true,
            extra_headers: std::collections::HashMap::new(),
        };

        let mut stream = client.send_stream(req).await.unwrap();

        // First chunk arrives promptly (well within the idle window).
        let first = stream
            .next()
            .await
            .expect("first chunk should arrive");
        assert!(first.is_ok(), "first chunk should be Ok: {:?}", first);

        // Bound the test so a regression (timeout never fires) fails fast
        // instead of hanging the suite.
        let second = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        match second {
            Ok(Some(Err(e))) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("stalled") || msg.contains("idle"),
                    "expected idle-timeout error, got: {msg}"
                );
            }
            Ok(Some(Ok(_))) => panic!("expected idle-timeout error, got another data chunk"),
            Ok(None) => panic!("expected idle-timeout error, stream ended cleanly instead"),
            Err(_) => panic!("idle timeout did not fire within 5s"),
        }
    }

    // -----------------------------------------------------------------------
    // Source guard checks
    // -----------------------------------------------------------------------

    /// Handler that sends partial data then drops the connection (upstream disconnect).
    async fn partial_disconnect_handler() -> axum::response::Response {
        let stream = futures::stream::unfold(0u32, |i| async move {
            if i >= 2 {
                // Stop sending -- simulates upstream dropping the connection.
                None
            } else {
                let chunk = format!("data: chunk {}\n\n", i);
                Some((
                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from(chunk)),
                    i + 1,
                ))
            }
        });
        (
            StatusCode::OK,
            [("Content-Type", "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response()
    }

    #[tokio::test]
    async fn upstream_disconnect_ends_stream_gracefully() {
        let app = Router::new().route("/test", post(partial_disconnect_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: true,
            extra_headers: std::collections::HashMap::new(),
        };

        let mut stream = client.send_stream(req).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => chunks.push(bytes),
                Err(e) => {
                    // Mid-stream errors are also acceptable.
                    panic!("unexpected mid-stream error: {:?}", e);
                }
            }
        }
        // Stream should have ended with the two chunks the server sent.
        assert!(
            !chunks.is_empty(),
            "Expected at least one chunk before upstream disconnect"
        );
        let combined = chunks.concat();
        let all_text = String::from_utf8_lossy(&combined);
        assert!(all_text.contains("chunk 0"), "Should have received chunk 0");
        assert!(all_text.contains("chunk 1"), "Should have received chunk 1");
    }

    /// Handler that sends some data then returns an error body mid-stream.
    async fn mid_stream_error_handler() -> axum::response::Response {
        // We simulate a mid-stream error by sending two successful chunks
        // then ending the stream. The consumer should see the data.
        // (A true mid-stream TCP error is hard to simulate with axum;
        // this test verifies the stream terminates correctly.)
        let stream = futures::stream::iter(vec![
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from("data: first\n\n")),
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from("data: second\n\n")),
        ]);
        (
            StatusCode::OK,
            [("Content-Type", "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response()
    }

    #[tokio::test]
    async fn stream_collects_all_chunks_then_ends() {
        let app = Router::new().route("/test", post(mid_stream_error_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: true,
            extra_headers: std::collections::HashMap::new(),
        };

        let mut stream = client.send_stream(req).await.unwrap();
        let mut all_data = Vec::new();
        while let Some(item) = stream.next().await {
            let bytes = item.expect("chunk should be Ok");
            all_data.push(bytes);
        }
        assert_eq!(all_data.len(), 2, "Expected exactly 2 chunks");
        assert!(String::from_utf8_lossy(&all_data[0]).contains("first"));
        assert!(String::from_utf8_lossy(&all_data[1]).contains("second"));
    }

    #[tokio::test]
    async fn empty_url_returns_http_error() {
        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: String::new(),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let err = client.send(req).await.unwrap_err();
        match err {
            ProviderError::Http { .. } => {} // expected
            other => panic!(
                "expected ProviderError::Http for empty URL, got: {:?}",
                other
            ),
        }
    }

    #[tokio::test]
    async fn large_body_sent_successfully() {
        // Use a body size that exceeds axum's default 2 MB limit would reject,
        // but is still reasonable. We configure the test server with a larger
        // body limit to verify the transport does not buffer or truncate.
        let app = axum::Router::new()
            .route("/test", post(echo_handler))
            .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        // 5 MB body
        let large_body = "x".repeat(5 * 1024 * 1024);
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: large_body.clone().into_bytes(),
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains(&large_body),
            "Response must echo the full large body back"
        );
    }

    // -----------------------------------------------------------------------
    // Source guard checks (original)
    // -----------------------------------------------------------------------

    /// Return only the production (non-test) portion of the source file.
    /// The guard tests check for forbidden identifiers in production code,
    /// not in the test assertions that mention those identifiers by name.
    fn prod_source() -> &'static str {
        let source = include_str!("transport.rs");
        // Split at the test module boundary.
        source
            .split_once("#[cfg(test)]")
            .map(|(prod, _)| prod)
            .unwrap_or(source)
    }

    #[test]
    fn transport_source_no_endpoint_classification() {
        let source = prod_source();
        assert!(
            !source.contains("EndpointType"),
            "transport.rs must not mention EndpointType"
        );
        assert!(
            !source.contains("classify_endpoint"),
            "transport.rs must not mention classify_endpoint"
        );
    }

    #[test]
    fn transport_source_no_provider_model() {
        let source = prod_source();
        assert!(
            !source.contains("OpenCodeClient"),
            "transport.rs must not mention OpenCodeClient"
        );
        assert!(
            !source.contains("opencode_go"),
            "transport.rs must not mention provider names"
        );
        assert!(
            !source.contains("opencode_zen"),
            "transport.rs must not mention provider names"
        );
        assert!(
            !source.contains("is_anthropic_model"),
            "transport.rs must not mention model classification"
        );
        assert!(
            !source.contains("is_gemini_model"),
            "transport.rs must not mention model classification"
        );
        assert!(
            !source.contains("is_responses_model"),
            "transport.rs must not mention model classification"
        );
    }

    #[test]
    fn transport_source_no_protocol_import() {
        let source = prod_source();
        assert!(
            !source.contains("llm_proxy_protocol"),
            "transport.rs must not import llm_proxy_protocol"
        );
    }

    // -----------------------------------------------------------------------
    // Edge case tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn empty_body_sent_successfully() {
        let app = Router::new().route("/test", post(echo_handler));
        let base = start_test_server(app).await;

        let client = ProxyClient::new();
        let req = ProxyRequest {
            url: format!("{}/test", base),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let resp = client.send(req).await.unwrap();
        let text = String::from_utf8(resp).unwrap();
        assert!(
            text.contains("body: "),
            "Response must echo body field even when empty: {}",
            text
        );
    }

    #[tokio::test]
    async fn unreachable_url_returns_http_error() {
        // With connect_timeout(10s) on ProxyClient, this should fail within
        // ~10 seconds rather than waiting for the OS TCP timeout (120+ s).
        let client = ProxyClient::new();
        let req = ProxyRequest {
            // Non-routable address guarantees a connection failure.
            url: "http://192.0.2.1:1/test".to_owned(),
            auth: AuthHeaders {
                style: AuthStyle::Bearer,
                api_key: "key".to_owned(),
            },
            body: vec![],
            stream: false,
            extra_headers: std::collections::HashMap::new(),
        };

        // Bound the test to 15 seconds as a safety net.
        let result = tokio::time::timeout(Duration::from_secs(15), client.send(req)).await;
        let err = match result {
            Ok(Err(e)) => e,
            Ok(Ok(_)) => panic!("expected error for unreachable URL, got success"),
            Err(_) => panic!("test timed out waiting for connection failure"),
        };
        match err {
            ProviderError::Http { .. } => {} // expected
            other => panic!(
                "expected ProviderError::Http for unreachable URL, got: {:?}",
                other
            ),
        }
    }
}
