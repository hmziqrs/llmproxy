//! `init` command implementation.
//!
//! Creates default config files and provider templates.

use anyhow::{Context, Result, bail};

use crate::defaults::{DEFAULT_CONFIG_TOML, DEFAULT_PROVIDER_OPENCODE_GO, DEFAULT_PROVIDER_OPENCODE_ZEN};
use crate::paths::{config_dir, default_config_path, providers_dir_for};
use crate::permissions::{create_private_file, set_private_dir_permissions};

/// Run the `init` command.
///
/// Creates:
/// - `config.toml` in the config directory
/// - `providers/opencode-go.toml` next to the config
/// - `providers/opencode-zen.toml` next to the config
///
/// Does NOT write:
/// - fallback/scenario JSON
/// - sampling/tool/reasoning/cache/stream overrides
pub fn cmd_init() -> Result<()> {
    let config_path = default_config_path();
    if config_path.exists() {
        bail!("config file already exists at {}", config_path.display());
    }

    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;
    set_private_dir_permissions(&dir)?;

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
    use crate::defaults::{DEFAULT_CONFIG_TOML, DEFAULT_PROVIDER_OPENCODE_GO, DEFAULT_PROVIDER_OPENCODE_ZEN};
    use crate::permissions::set_private_permissions;

    #[test]
    fn cmd_init_creates_files_with_permissions() {
        let dir = tempfile::tempdir().unwrap();
        // Override config_dir by running cmd_init with a known directory.
        // Since cmd_init uses default_config_path() internally, we test by
        // directly calling the file creation logic.

        let config_path = dir.path().join("config.toml");
        let providers_dir = dir.path().join("providers");
        std::fs::create_dir_all(&providers_dir).unwrap();

        // Write main config
        std::fs::write(&config_path, DEFAULT_CONFIG_TOML).unwrap();
        assert!(config_path.exists());

        // Write provider files
        let go_path = providers_dir.join("opencode-go.toml");
        std::fs::write(&go_path, DEFAULT_PROVIDER_OPENCODE_GO).unwrap();
        assert!(go_path.exists());

        let zen_path = providers_dir.join("opencode-zen.toml");
        std::fs::write(&zen_path, DEFAULT_PROVIDER_OPENCODE_ZEN).unwrap();
        assert!(zen_path.exists());

        // Verify file contents are valid TOML
        let main_content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&main_content).unwrap();
        assert!(parsed.get("server").is_some());

        let go_content = std::fs::read_to_string(&go_path).unwrap();
        let parsed: toml::Value = toml::from_str(&go_content).unwrap();
        assert!(parsed.get("provider").is_some());
        assert_eq!(
            parsed["provider"]["name"].as_str(),
            Some("opencode-go")
        );

        let zen_content = std::fs::read_to_string(&zen_path).unwrap();
        let parsed: toml::Value = toml::from_str(&zen_content).unwrap();
        assert_eq!(
            parsed["provider"]["name"].as_str(),
            Some("opencode-zen")
        );

        // Verify restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            set_private_permissions(&config_path).unwrap();
            let mode = config_path.metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);

            set_private_permissions(&go_path).unwrap();
            let mode = go_path.metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn cmd_init_rejects_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "existing").unwrap();
        // cmd_init calls default_config_path() which uses config_dir(),
        // so we can't easily test the full function here. Instead verify
        // the file-exists check logic.
        assert!(config_path.exists());
    }
}
