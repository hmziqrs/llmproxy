//! llm-proxy-api: public API surface for the LLM proxy.
//!
//! This crate is intentionally empty in v1. It reserves the namespace for a
//! future public HTTP API layer. See `docs/protocol.md` for the implemented
//! protocol boundaries.

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
