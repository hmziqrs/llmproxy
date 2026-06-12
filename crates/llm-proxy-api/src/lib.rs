//! llm-proxy-api: public API surface for the LLM proxy.
//!
//! This crate is intentionally empty in v1. It will contain the
//! HTTP handler layer, request/response types, and middleware once
//! the protocol-normalization and protocol-mini designs land.
//!
//! See `docs/protocol-normalization.md` and `docs/protocol-mini.md`
//! for the planned API contract.

/// Placeholder module to reserve the public API namespace.
///
/// Remove this module and replace with real types once implementation
/// begins. Tracking: <https://github.com/hmziq/llm-proxy/issues>.
pub mod placeholder {
    /// Sentinel type indicating this crate has not yet been implemented.
    ///
    /// This type exists so that downstream crates can reference
    /// `llm_proxy_api::placeholder::NotImplemented` in feature-gated
    /// code without pulling in a fully implemented crate.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct NotImplemented;
}

// Ensure the crate compiles as a valid library target.
#[cfg(test)]
mod tests {
    use super::placeholder::NotImplemented;

    #[test]
    fn placeholder_type_exists() {
        let _ = NotImplemented;
    }
}
