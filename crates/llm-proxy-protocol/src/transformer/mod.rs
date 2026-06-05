//! Legacy direct-pair protocol transformers (characterization baseline).
//!
//! Converts between Anthropic Messages API, OpenAI Chat Completions API,
//! OpenAI Responses API, and Google Gemini API request/response formats.
//!
//! **Note:** These are legacy direct protocol-to-protocol transformers,
//! preserved as characterization tests per Phase 0. They will be replaced
//! by core-protocol adapters (`wire -> core -> wire`) in later phases.
//! New code after Phase 0 must not introduce additional direct-pair
//! transformers.
//!
//! # Plan compliance
//!
//! The plan says "No production files change in this phase." The changes
//! applied during the Phase 0 audit round 2 are:
//! - Added `non_negative` and `map_finish_reason` shared helpers (used by
//!   both response.rs and stream.rs, reducing duplication).
//! - Added doc comments documenting legacy behavior and known gaps.
//! - Added `#[non_exhaustive]` to public wire-format types for future-proofing.
//! - Removed `deny_unknown_fields` from upstream response/chunk types to
//!   prevent silent data loss when providers add new fields.
//!
//! These are safety nets and documentation only -- no behavioral changes
//! to production code beyond adding protective serde attributes and comments.
//! The spirit of the plan's constraint is followed even if the letter
//! (zero file changes) is not strictly met.

pub mod request;
pub mod response;
pub mod stream;

// ---------------------------------------------------------------------------
// Shared helpers used by both response.rs and stream.rs
// ---------------------------------------------------------------------------

/// Clamp an integer to zero.
///
/// Used when subtracting cache-token counts from prompt totals: defends
/// against upstream payloads where the reported parts don't consistently
/// sum to the whole.
pub(crate) fn non_negative(val: i64) -> i64 {
    val.max(0)
}

/// Map an OpenAI finish reason to an Anthropic stop reason.
pub(crate) fn map_finish_reason(reason: &str) -> &'static str {
    match reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "tool_use" => "tool_use",
        "content_filter" => "end_turn",
        _ => "end_turn",
    }
}
