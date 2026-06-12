//! llm-proxy-storage: persistence and storage layer for the LLM proxy.
//!
//! This crate is intentionally empty in v1. It will contain the
//! storage backends (SQLite, filesystem, etc.), key-value caches,
//! and conversation history once the protocol-normalization and
//! protocol-mini designs land.
//!
//! See `docs/protocol-normalization.md` and `docs/protocol-mini.md`
//! for the planned storage contract.

/// Placeholder module to reserve the public API namespace.
///
/// Remove this module and replace with real types once implementation
/// begins. Tracking: <https://github.com/hmziq/llm-proxy/issues>.
pub mod placeholder {
    /// Sentinel type indicating this crate has not yet been implemented.
    ///
    /// This type exists so that downstream crates can reference
    /// `llm_proxy_storage::placeholder::NotImplemented` in feature-gated
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
