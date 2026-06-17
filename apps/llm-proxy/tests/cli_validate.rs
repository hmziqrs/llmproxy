//! Integration tests for the `validate` command.
//!
//! Covers end-to-end `cmd_validate` behaviour: valid TOML, invalid TOML, and
//! the wrong-extension error surfacing through the command. The pure extension
//! accept/reject matrix is exercised as per-case unit tests in
//! `src/config_validation.rs` (see LOW-20).

use llm_proxy_app::defaults::DEFAULT_CONFIG_TOML;
use llm_proxy_app::commands::cmd_validate;

#[test]
fn validate_accepts_default_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, DEFAULT_CONFIG_TOML).unwrap();

    let result = cmd_validate(Some(config));
    assert!(result.is_ok(), "default config should validate: {:?}", result);
}

#[test]
fn validate_rejects_invalid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("bad.toml");
    std::fs::write(&config, "not valid toml {{{{").unwrap();

    let result = cmd_validate(Some(config));
    assert!(result.is_err(), "malformed TOML should fail");
}

#[test]
fn validate_rejects_json_extension() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, "{}").unwrap();

    let result = cmd_validate(Some(config));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("unsupported") || msg.contains("extension"),
        "error should mention unsupported extension: {msg}"
    );
}
