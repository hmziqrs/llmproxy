//! Integration smoke test: verifies that the core module is publicly
//! exported and the primary types can be imported.
//!
//! This prevents the Phase 1 gate from passing if `core.rs` or
//! `pub mod core;` is missing.

use llm_proxy_protocol::core::{CoreEvent, CoreRequest, CoreResponse};

/// Trivial compile-time check: the types exist and are constructible.
#[test]
fn core_types_are_exported() {
    let _request: CoreRequest;
    let _response: CoreResponse;
    let _event: CoreEvent;
}
