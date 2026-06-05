//! Integration smoke test: verifies that the core module is publicly
//! exported and the primary types can be imported, constructed, and
//! serialized.
//!
//! This prevents the Phase 1 gate from passing if `core.rs` or
//! `pub mod core;` is missing.

use llm_proxy_protocol::core::{
    CacheControl, CacheControlType, ContentKind, CoreEvent, CoreRequest, CoreResponse, CoreRole,
    CoreMessage, CoreContent, CoreStreamError, CoreStreamErrorKind, CoreTool, CoreToolChoice,
    ModelRef, RequestMetadata, SamplingOptions, ProviderHints, StopReason, Usage, UsageProvenance,
};

/// Trivial compile-time check: the types exist and are constructible.
#[test]
fn core_types_are_exported() {
    let _request: CoreRequest;
    let _response: CoreResponse;
    let _event: CoreEvent;
    let _content_kind: ContentKind;
    let _stream_error: CoreStreamError;
    let _stream_error_kind: CoreStreamErrorKind;
    let _tool: CoreTool;
    let _tool_choice: CoreToolChoice;
    let _stop_reason: StopReason;
    let _usage: Usage;
    let _provenance: UsageProvenance;
}

/// Stronger check: construct a minimal CoreRequest and verify serialization.
#[test]
fn core_request_serializes() {
    let req = CoreRequest {
        model: ModelRef {
            requested: "test-model".into(),
            upstream: None,
        },
        system: vec![CoreContent::Text {
            text: "be helpful".into(),
            cache: None,
        }],
        messages: vec![CoreMessage {
            role: CoreRole::User,
            content: vec![CoreContent::Text {
                text: "hello".into(),
                cache: None,
            }],
        }],
        tools: vec![],
        tool_choice: None,
        sampling: SamplingOptions::default(),
        stream: false,
        metadata: RequestMetadata::default(),
        provider_hints: ProviderHints::default(),
    };
    let json = serde_json::to_string(&req).expect("CoreRequest should serialize");
    assert!(!json.is_empty());
    let back: CoreRequest = serde_json::from_str(&json).expect("CoreRequest should deserialize");
    assert_eq!(back.model.requested, "test-model");
}

/// Verify CacheControlType enum is exported and works.
#[test]
fn cache_control_type_is_exported() {
    let cc = CacheControl {
        r#type: CacheControlType::Ephemeral,
    };
    assert_eq!(serde_json::to_string(&cc).unwrap(), r#"{"type":"ephemeral"}"#);
}
