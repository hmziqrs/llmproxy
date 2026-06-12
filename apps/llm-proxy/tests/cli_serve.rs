//! Integration tests for the `serve` command.
//!
//! Since starting the server binds a port and blocks, we only test the
//! pre-flight validation logic (config resolution, extension check) without
//! actually starting the server.

use llm_proxy_app::config_validation::validate_toml_extension;

#[test]
fn serve_rejects_non_toml_config() {
    let result = validate_toml_extension(std::path::Path::new("config.json"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unsupported"), "expected 'unsupported' in error: {msg}");
}

#[test]
fn serve_rejects_no_extension() {
    let result = validate_toml_extension(std::path::Path::new("config"));
    assert!(result.is_err());
}

#[test]
fn serve_accepts_toml_extension() {
    let result = validate_toml_extension(std::path::Path::new("my-config.toml"));
    assert!(result.is_ok());
}

#[test]
fn serve_accepts_uppercase_toml() {
    let result = validate_toml_extension(std::path::Path::new("CONFIG.TOML"));
    assert!(result.is_ok());
}

#[test]
fn serve_accepts_path_with_directory() {
    let result = validate_toml_extension(std::path::Path::new("/etc/llm-proxy/config.toml"));
    assert!(result.is_ok());
}
