//! `init` command implementation.
//!
//! Creates default config files and provider templates.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::defaults::{
    DEFAULT_CONFIG_TOML, DEFAULT_PROVIDER_OPENCODE_GO, DEFAULT_PROVIDER_OPENCODE_ZEN,
};
use crate::paths::{config_dir, providers_dir_for};
use crate::permissions::{create_private_file, set_private_dir_permissions};

/// Run the `init` command against the default config directory.
///
/// Thin wrapper around [`cmd_init_at`] using [`config_dir`] as the base, kept
/// as the zero-arg entry point wired into the CLI dispatch.
pub fn cmd_init() -> Result<()> {
    cmd_init_at(&config_dir())
}

/// Run the `init` command against an explicit base directory.
///
/// Creates, under `base_dir`:
/// - `config.toml`
/// - `providers/opencode-go.toml`
/// - `providers/opencode-zen.toml`
///
/// Does NOT write:
/// - fallback/scenario JSON
/// - sampling/tool/reasoning/cache/stream overrides
///
/// Accepting the base directory as a parameter (rather than hard-coding
/// `config_dir()`) lets the full control flow -- the overwrite guard,
/// `create_dir_all` + `set_private_dir_permissions` ordering, and the
/// sequential born-private provider writes -- be exercised against a
/// tempdir in tests instead of the process-global config directory (audit
/// finding app-cli:init-test-does-not-exercise-cmd-init).
pub(super) fn cmd_init_at(base_dir: &Path) -> Result<()> {
    let config_path: PathBuf = base_dir.join("config.toml");
    if config_path.exists() {
        bail!("config file already exists at {}", config_path.display());
    }

    std::fs::create_dir_all(base_dir)
        .with_context(|| format!("creating directory {}", base_dir.display()))?;
    set_private_dir_permissions(base_dir)?;

    // Write main config.toml with restrictive permissions (contains env var references).
    create_private_file(&config_path, DEFAULT_CONFIG_TOML.as_bytes())?;
    println!("created config at {}", config_path.display());

    // Create providers directory.
    let providers_dir = providers_dir_for(&config_path);
    std::fs::create_dir_all(&providers_dir)
        .with_context(|| format!("creating directory {}", providers_dir.display()))?;

    // Write opencode-go provider with restrictive permissions.
    let go_path = providers_dir.join("opencode-go.toml");
    create_private_file(&go_path, DEFAULT_PROVIDER_OPENCODE_GO.as_bytes())?;
    println!("created provider config at {}", go_path.display());

    // Write opencode-zen provider with restrictive permissions.
    let zen_path = providers_dir.join("opencode-zen.toml");
    create_private_file(&zen_path, DEFAULT_PROVIDER_OPENCODE_ZEN.as_bytes())?;
    println!("created provider config at {}", zen_path.display());

    println!();
    println!("Next steps:");
    println!("  1. Set API keys via environment variables:");
    println!("     export LLM_PROXY_OPENCODE_GO_KEY=your-go-key");
    println!("     export LLM_PROXY_OPENCODE_ZEN_KEY=your-zen-key");
    println!("  2. Run `llm-proxy validate` to check your config");
    println!("  3. Run `llm-proxy serve` to start the proxy");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cmd_init_at;

    /// Run the real `cmd_init_at` against a tempdir and verify it creates all
    /// expected files with valid TOML contents and born-private permissions.
    ///
    /// Unlike the previous version of this test (which manually `std::fs::write`d
    /// the files and never invoked `cmd_init`), this exercises the actual
    /// command flow: the overwrite guard, `create_dir_all` +
    /// `set_private_dir_permissions`, and the sequential `create_private_file`
    /// provider writes.
    #[test]
    fn cmd_init_creates_files_with_permissions() {
        let dir = tempfile::tempdir().unwrap();
        cmd_init_at(dir.path()).expect("cmd_init_at should succeed on an empty base dir");

        let config_path = dir.path().join("config.toml");
        let providers_dir = dir.path().join("providers");
        let go_path = providers_dir.join("opencode-go.toml");
        let zen_path = providers_dir.join("opencode-zen.toml");

        assert!(config_path.exists(), "config.toml was created");
        assert!(go_path.exists(), "opencode-go.toml was created");
        assert!(zen_path.exists(), "opencode-zen.toml was created");

        // Verify file contents are valid TOML with the expected structure.
        let main_content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value =
            toml::from_str(&main_content).expect("generated config.toml must be valid TOML");
        assert!(
            parsed.get("server").is_some(),
            "generated config.toml must have a [server] section"
        );

        let go_content = std::fs::read_to_string(&go_path).unwrap();
        let parsed: toml::Value = toml::from_str(&go_content).unwrap();
        assert!(parsed.get("provider").is_some());
        assert_eq!(parsed["provider"]["name"].as_str(), Some("opencode-go"));

        let zen_content = std::fs::read_to_string(&zen_path).unwrap();
        let parsed: toml::Value = toml::from_str(&zen_content).unwrap();
        assert_eq!(parsed["provider"]["name"].as_str(), Some("opencode-zen"));

        // Verify restrictive permissions on Unix via the real create_private_file
        // path (the atomic 0600 open), not a separate set_private_permissions call.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = config_path.metadata().unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "config.toml should be owner-only (0600)"
            );
            let mode = go_path.metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = zen_path.metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// Verify `cmd_init_at` bails when a config already exists and does NOT
    /// overwrite the existing file. This exercises the real overwrite guard
    /// at the top of the command (previously a tautology that only asserted a
    /// file exists right after writing it).
    #[test]
    fn cmd_init_rejects_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(&config_path, "existing").unwrap();

        let result = cmd_init_at(dir.path());
        let err = result.expect_err("cmd_init_at must error when config.toml already exists");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already exists"),
            "error should mention the config already exists, got: {msg}"
        );

        // The existing file must be left untouched.
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "existing",
            "cmd_init_at must not overwrite an existing config"
        );

        // And the provider files must NOT have been created either, since the
        // guard bails before any write.
        let go_path = dir.path().join("providers").join("opencode-go.toml");
        assert!(
            !go_path.exists(),
            "no provider files should be written on bail"
        );
    }

    /// A second `cmd_init_at` on the same (now-populated) directory must bail
    /// rather than silently clobbering the operator's config. Guards the
    /// "re-running init" UX path end-to-end.
    #[test]
    fn cmd_init_is_idempotent_reject() {
        let dir = tempfile::tempdir().unwrap();
        cmd_init_at(dir.path()).expect("first init should succeed");
        let second = cmd_init_at(dir.path());
        assert!(
            second.is_err(),
            "a second init against a populated dir must bail, not clobber"
        );
    }
}
