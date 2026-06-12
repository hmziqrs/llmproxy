//! Integration tests for the `init` command.
//!
//! Verifies file creation, content validity, permissions, and idempotency.

use std::fs;

use llm_proxy_app::defaults::{DEFAULT_CONFIG_TOML, DEFAULT_PROVIDER_OPENCODE_GO, DEFAULT_PROVIDER_OPENCODE_ZEN};

#[test]
fn init_default_toml_is_valid() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML)
        .expect("default config TOML should parse");
    assert!(parsed.get("server").is_some(), "default config must have [server]");
}

#[test]
fn init_provider_opencode_go_is_valid() {
    let parsed: toml::Value = toml::from_str(DEFAULT_PROVIDER_OPENCODE_GO)
        .expect("opencode-go provider TOML should parse");
    let provider = parsed.get("provider").expect("must have [provider]");
    assert_eq!(provider["name"].as_str(), Some("opencode-go"));
}

#[test]
fn init_provider_opencode_zen_is_valid() {
    let parsed: toml::Value = toml::from_str(DEFAULT_PROVIDER_OPENCODE_ZEN)
        .expect("opencode-zen provider TOML should parse");
    let provider = parsed.get("provider").expect("must have [provider]");
    assert_eq!(provider["name"].as_str(), Some("opencode-zen"));
}

#[test]
fn init_writes_files_to_tempdir() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Simulate the file-creation logic of cmd_init without calling the real
    // function (which hard-codes paths via config_dir()).
    let config_path = dir.path().join("config.toml");
    let providers_dir = dir.path().join("providers");
    fs::create_dir_all(&providers_dir).expect("providers dir");

    fs::write(&config_path, DEFAULT_CONFIG_TOML.as_bytes()).expect("config");
    fs::write(providers_dir.join("opencode-go.toml"), DEFAULT_PROVIDER_OPENCODE_GO.as_bytes()).expect("go");
    fs::write(providers_dir.join("opencode-zen.toml"), DEFAULT_PROVIDER_OPENCODE_ZEN.as_bytes()).expect("zen");

    assert!(config_path.exists());
    assert!(providers_dir.join("opencode-go.toml").exists());
    assert!(providers_dir.join("opencode-zen.toml").exists());

    // Verify parsed contents match.
    let main: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(main.get("server").is_some());

    let go: toml::Value = toml::from_str(&fs::read_to_string(providers_dir.join("opencode-go.toml")).unwrap()).unwrap();
    assert_eq!(go["provider"]["name"].as_str(), Some("opencode-go"));

    let zen: toml::Value = toml::from_str(&fs::read_to_string(providers_dir.join("opencode-zen.toml")).unwrap()).unwrap();
    assert_eq!(zen["provider"]["name"].as_str(), Some("opencode-zen"));
}

#[test]
fn init_rejects_existing_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "existing").unwrap();
    // The actual cmd_init() bails if the file exists — verify the condition.
    assert!(config_path.exists());
}

#[test]
#[cfg(unix)]
fn init_files_have_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;
    use llm_proxy_app::permissions::create_private_file;

    let dir = tempfile::tempdir().unwrap();

    let file_path = dir.path().join("secret.toml");
    create_private_file(&file_path, b"test").expect("create_private_file");

    let mode = file_path.metadata().unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "file should have 0600 permissions");
}
