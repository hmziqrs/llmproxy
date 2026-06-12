//! File permission helpers.
//!
//! Provides functions to set restrictive owner-only permissions on files
//! and directories containing sensitive configuration (API key references).

use std::io::Write;

use anyhow::{Context, Result};

/// Set owner-only read/write permissions (0600) on a file.
///
/// Used for config and autostart files that reference API keys via
/// environment variables. On non-Unix platforms this is a no-op.
pub fn set_private_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Set owner-only permissions (0700) on a directory.
///
/// Used for the config directory which contains provider TOML files with
/// API key references. On non-Unix platforms this is a no-op.
pub fn set_private_dir_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Create a new file with restrictive permissions (0600) and write `content`.
///
/// On Unix, uses `OpenOptions` with `mode(0o600)` to set permissions atomically
/// at creation time, avoiding a window where the file exists with default umask
/// permissions. Falls back to create-then-chmod on non-Unix.
pub fn create_private_file(path: &std::path::Path, content: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        f.write_all(content)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::File::create(path)
            .with_context(|| format!("creating {}", path.display()))?;
        f.write_all(content)
            .with_context(|| format!("writing {}", path.display()))?;
        set_private_permissions(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_private_file_writes_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        create_private_file(&path, b"hello world").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = path.metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn create_private_file_rejects_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, "existing").unwrap();
        let result = create_private_file(&path, b"new content");
        assert!(result.is_err());
    }
}
