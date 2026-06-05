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
#[derive(Debug, thiserror::Error)]
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
}
