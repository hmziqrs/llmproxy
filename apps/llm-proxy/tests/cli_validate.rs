//! Integration tests for the `validate` command.
//!
//! Covers valid TOML, invalid TOML, wrong extension, and edge cases.

use std::path::Path;

use llm_proxy_app::config_validation::{is_toml_config, validate_toml_extension};
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

#[test]
fn validate_rejects_no_extension() {
    let result = validate_toml_extension(Path::new("config"));
    assert!(result.is_err());
}

#[test]
fn validate_accepts_uppercase_toml() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.TOML");
    std::fs::write(&config, DEFAULT_CONFIG_TOML).unwrap();

    // Extension check should pass.
    assert!(is_toml_config(&config));
    assert!(validate_toml_extension(&config).is_ok());
}

#[test]
fn is_toml_config_various() {
    assert!(is_toml_config(Path::new("a.toml")));
    assert!(is_toml_config(Path::new("a.TOML")));
    assert!(!is_toml_config(Path::new("a.json")));
    assert!(!is_toml_config(Path::new("a")));
    assert!(!is_toml_config(Path::new("a.toml.bak")));
}
