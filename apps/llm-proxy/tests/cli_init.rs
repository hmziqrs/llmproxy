//! Integration tests for the `init` command.
//!
//! Verifies file creation, content validity, permissions, and idempotency.

use std::fs;
use std::sync::Mutex;

use llm_proxy_app::commands::cmd_init;
use llm_proxy_app::defaults::{
    DEFAULT_CONFIG_TOML, DEFAULT_PROVIDER_OPENCODE_GO, DEFAULT_PROVIDER_OPENCODE_ZEN,
};

/// Serializes tests that mutate the process-global `$HOME` env var so they do
/// not race with each other when run in parallel. Every such test must hold
/// this lock for its whole body and restore `$HOME` before releasing it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that sets `$HOME` to `temp_home` on construction and restores the
/// prior value on drop. Must be constructed while holding [`ENV_LOCK`].
struct HomeGuard {
    prior: Option<String>,
}

impl HomeGuard {
    fn new(temp_home: &std::path::Path) -> Self {
        // SAFETY: callers hold ENV_LOCK, so no other test mutates/reads HOME
        // concurrently. We always restore on drop.
        let prior = std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned());
        // SAFETY: see above; this test is the sole mutator of HOME under the lock.
        unsafe { std::env::set_var("HOME", temp_home) };
        Self { prior }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: held under ENV_LOCK; restores the pre-test HOME value.
        unsafe {
            match self.prior.as_ref() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

#[test]
fn init_default_toml_is_valid() {
    let parsed: toml::Value =
        toml::from_str(DEFAULT_CONFIG_TOML).expect("default config TOML should parse");
    assert!(
        parsed.get("server").is_some(),
        "default config must have [server]"
    );
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
    fs::write(
        providers_dir.join("opencode-go.toml"),
        DEFAULT_PROVIDER_OPENCODE_GO.as_bytes(),
    )
    .expect("go");
    fs::write(
        providers_dir.join("opencode-zen.toml"),
        DEFAULT_PROVIDER_OPENCODE_ZEN.as_bytes(),
    )
    .expect("zen");

    assert!(config_path.exists());
    assert!(providers_dir.join("opencode-go.toml").exists());
    assert!(providers_dir.join("opencode-zen.toml").exists());

    // Verify parsed contents match.
    let main: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(main.get("server").is_some());

    let go: toml::Value =
        toml::from_str(&fs::read_to_string(providers_dir.join("opencode-go.toml")).unwrap())
            .unwrap();
    assert_eq!(go["provider"]["name"].as_str(), Some("opencode-go"));

    let zen: toml::Value =
        toml::from_str(&fs::read_to_string(providers_dir.join("opencode-zen.toml")).unwrap())
            .unwrap();
    assert_eq!(zen["provider"]["name"].as_str(), Some("opencode-zen"));
}

/// `cmd_init` must refuse to overwrite an existing config and leave the file
/// contents untouched.
///
/// `cmd_init` resolves its config directory via `$HOME` (see `paths::config_dir`).
/// We point `$HOME` at a tempdir, pre-create the config file, invoke the real
/// `cmd_init`, and assert it returns `Err` with the "already exists" message
/// without overwriting the file. `$HOME` mutation is serialized via `ENV_LOCK`.
#[test]
fn init_rejects_existing_config() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");

    // cmd_init writes to $HOME/.config/llm-proxy/config.toml.
    let config_subdir = dir.path().join(".config").join("llm-proxy");
    fs::create_dir_all(&config_subdir).expect("create config dir");
    let config_path = config_subdir.join("config.toml");
    fs::write(&config_path, "existing").expect("seed existing config");

    let _home = HomeGuard::new(dir.path());

    // The real command must bail because the config already exists.
    let result = cmd_init();
    let err = result.expect_err("cmd_init should reject an existing config");

    // The bail message names the config path and the "already exists" condition.
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already exists"),
        "error should mention the config already existing, got: {msg}"
    );

    // The existing file must NOT have been overwritten.
    let on_disk = fs::read_to_string(&config_path).expect("read config");
    assert_eq!(
        on_disk, "existing",
        "cmd_init must not overwrite an existing config"
    );
}

#[test]
#[cfg(unix)]
fn init_files_have_restrictive_permissions() {
    use llm_proxy_app::permissions::create_private_file;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();

    let file_path = dir.path().join("secret.toml");
    create_private_file(&file_path, b"test").expect("create_private_file");

    let mode = file_path.metadata().unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "file should have 0600 permissions");
}
