//! Integration tests for the `serve` command.
//!
//! Starting the server binds a port and blocks, so these tests only cover the
//! pre-flight config validation that `serve` runs before it would bind. The
//! full `validate_toml_extension` accept/reject matrix is exercised as per-case
//! unit tests in `src/config_validation.rs` (see LOW-20); this file keeps a
//! single smoke test confirming the error shape `serve` surfaces.

use llm_proxy_app::config_validation::validate_toml_extension;

#[test]
fn serve_rejects_non_toml_config_with_unsupported_message() {
    let result = validate_toml_extension(std::path::Path::new("config.json"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unsupported"), "expected 'unsupported' in error: {msg}");
}
