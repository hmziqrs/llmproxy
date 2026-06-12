//! Client protocol adapters.
//!
//! Each adapter translates between a specific client wire format and the
//! normalised core protocol types.  Adapters do **not** import or call
//! provider code, server code, core config/routing, endpoint classification,
//! scenario/fallback code, or the transformer module.
//!
//! ## Design rules
//!
//! * Each adapter only knows: `its protocol <-> Core types`.
//! * No adapter may special-case another protocol's hints or wire format.
//! * HTTP framing (SSE, status codes) is a route concern, not an adapter concern.

pub mod anthropic;
pub mod openai_chat;

// ---------------------------------------------------------------------------
// ProtocolError
// ---------------------------------------------------------------------------

/// Errors returned by client protocol adapters.
///
/// Every adapter returns this type so that route handlers can map errors to
/// appropriate HTTP responses without depending on adapter-specific error types.
///
/// # HTTP status mapping
///
/// Route handlers should translate `ProtocolError` variants into HTTP responses
/// as follows:
///
/// - `InvalidRequest` -> 400 Bad Request
/// - `Encode` -> 500 Internal Server Error (or 502 Bad Gateway if the cause
///   is upstream data that cannot be represented in the client protocol)
/// - `Decode` -> 400 Bad Request (malformed client input)
///
/// `ProtocolError` does not carry an HTTP status code directly because the
/// route handler may need to override the status based on context (e.g.
/// streaming vs non-streaming, request-phase vs response-phase).
///
/// # Error source chain
///
/// `ProtocolError` variants hold only `String` messages, not source errors.
/// This is intentional: the protocol adapter layer consumes underlying
/// serde/JSON errors during translation and converts them to human-readable
/// strings. The original error type information and backtraces are lost at
/// this boundary. If richer error chains are needed in the future, consider
/// wrapping the source error with `#[source]` or using `Box<dyn std::error::Error>`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The client request could not be decoded into core types.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// An error occurred while encoding core types to the client protocol.
    #[error("encode error: {0}")]
    Encode(String),
    /// An error occurred while decoding the client protocol into core types.
    #[error("decode error: {0}")]
    Decode(String),
    /// A content block was dropped because the client protocol does not support
    /// it, but the omission is safe (e.g. Document/Audio/Video in Anthropic
    /// responses). The block was already logged with a `tracing::warn!`.
    #[error("skippable encode: {0}")]
    EncodeSkippable(String),
}
