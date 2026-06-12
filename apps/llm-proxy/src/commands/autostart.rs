//! `autostart` command implementations.
//!
//! Manages auto-start on login: enable, disable, and status.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::paths::config_dir;
#[cfg(target_os = "macos")]
use crate::paths::launchd_plist_path;
#[cfg(target_os = "linux")]
use crate::paths::linux_autostart_dir;
use crate::permissions::set_private_permissions;
#[cfg(target_os = "macos")]
use crate::platform::format_plist;
#[cfg(target_os = "linux")]
use crate::platform::format_desktop_entry;

/// Run the `autostart enable` command.
pub fn cmd_autostart_enable(config_path: Option<PathBuf>, port: Option<u16>) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;

    let exe = std::env::current_exe().with_context(|| "resolving current executable")?;
    // Build args as strings for plist/desktop entry display. Lossy conversion is
    // acceptable here because these are display-only strings that are XML-escaped
    // by format_plist(). Non-UTF-8 paths are rare and the lossy replacement is
    // sufficient for auto-start purposes.
    let mut args = vec![exe.to_string_lossy().to_string(), "serve".to_string()];

    if let Some(ref p) = config_path {
        args.push("--config".to_string());
        args.push(p.to_string_lossy().to_string());
    }
    if let Some(p) = port {
        args.push("--port".to_string());
        args.push(p.to_string());
    }

    #[cfg(target_os = "macos")]
    {
        let plist_content = format_plist(&args);
        let plist_path = launchd_plist_path();

        let plist_dir = plist_path
            .parent()
            .with_context(|| format!("invalid plist path: {}", plist_path.display()))?;
        std::fs::create_dir_all(plist_dir)
            .with_context(|| format!("creating {}", plist_dir.display()))?;

        std::fs::write(&plist_path, plist_content)
            .with_context(|| format!("writing {}", plist_path.display()))?;
        // Set restrictive permissions on the plist file.
        set_private_permissions(&plist_path)?;

        println!("created launchd plist at {}", plist_path.display());
        println!("run: launchctl load {}", plist_path.display());
    }

    #[cfg(target_os = "linux")]
    {
        let desktop_content = format_desktop_entry(&args);
        let autostart_dir = linux_autostart_dir();
        std::fs::create_dir_all(&autostart_dir)
            .with_context(|| format!("creating {}", autostart_dir.display()))?;
        let desktop_path = autostart_dir.join("llm-proxy.desktop");
        std::fs::write(&desktop_path, desktop_content)
            .with_context(|| format!("writing {}", desktop_path.display()))?;
        // Set restrictive permissions on the desktop entry.
        set_private_permissions(&desktop_path)?;
        println!("created desktop entry at {}", desktop_path.display());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("auto-start is not supported on this platform");
        println!("add this command to your shell startup script:");
        println!("  {}", args.join(" "));
    }

    Ok(())
}

/// Run the `autostart disable` command.
pub fn cmd_autostart_disable() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchd_plist_path();

        if plist_path.exists() {
            let unload_result = std::process::Command::new("launchctl")
                .args(["unload", &plist_path.to_string_lossy()])
                .output();
            match unload_result {
                Ok(output) if !output.status.success() => {
                    eprintln!(
                        "warning: launchctl unload failed (exit {:?}): {}",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                Err(e) => {
                    eprintln!("warning: failed to run launchctl unload: {e}");
                }
                _ => {}
            }
            std::fs::remove_file(&plist_path)
                .with_context(|| format!("removing {}", plist_path.display()))?;
            println!("removed launchd plist");
        } else {
            println!("no launchd plist found");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let autostart_dir = linux_autostart_dir();
        let desktop_path = autostart_dir.join("llm-proxy.desktop");
        if desktop_path.exists() {
            std::fs::remove_file(&desktop_path)
                .with_context(|| format!("removing {}", desktop_path.display()))?;
            println!("removed desktop entry");
        } else {
            println!("no desktop entry found");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("auto-start is not supported on this platform");
    }

    Ok(())
}

/// Run the `autostart status` command.
pub fn cmd_autostart_status() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchd_plist_path();

        if plist_path.exists() {
            println!("auto-start is enabled ({})", plist_path.display());
        } else {
            println!("auto-start is disabled (no launchd plist found)");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let autostart_dir = linux_autostart_dir();
        let desktop_path = autostart_dir.join("llm-proxy.desktop");
        if desktop_path.exists() {
            println!("auto-start is enabled ({})", desktop_path.display());
        } else {
            println!("auto-start is disabled (no desktop entry found)");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("auto-start is not supported on this platform");
    }

    Ok(())
}
