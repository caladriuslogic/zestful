//! Device ID bootstrap.
//!
//! Reads `~/.config/zestful/device.id`. If missing, mints a fresh ULID and
//! writes it with mode 0600 (Unix). The file is intended to be stable for
//! the lifetime of the machine's Zestful install.

use crate::config;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

const DEVICE_FILE: &str = "device.id";

/// Return the stable device ID, creating it on first call if needed.
///
/// On read/write errors, returns a session-scoped fallback ULID so emitters
/// never fail because of device-id problems. The fallback is not persisted.
pub fn device_id() -> String {
    let path = device_file_path();

    // Fast path: existing file
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Slow path: mint and persist
    let new_id = ulid::Ulid::new().to_string();
    if let Err(e) = write_device_file(&path, &new_id) {
        // Couldn't write — fall back to session-scoped id.
        crate::log::log(
            "events",
            &format!("device.id write failed ({}): using session-scoped id", e),
        );
    }
    new_id
}

fn device_file_path() -> PathBuf {
    config::config_dir().join(DEVICE_FILE)
}

fn write_device_file(path: &std::path::Path, id: &str) -> std::io::Result<()> {
    // Ensure parent dir exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Atomic create-new: fail if another process raced us and already wrote.
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            f.write_all(id.as_bytes())?;
            f.write_all(b"\n")?;
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            // Another process wrote it first — fine.
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Redirect `config::config_dir` to a temp dir by setting HOME (Unix) or
    /// USERPROFILE (Windows). Restore on drop.
    struct HomeGuard {
        old_home: Option<String>,
        _td: TempDir,
    }

    impl HomeGuard {
        fn new() -> (Self, PathBuf) {
            let td = TempDir::new().unwrap();
            let home_var = if cfg!(target_os = "windows") {
                "USERPROFILE"
            } else {
                "HOME"
            };
            let old_home = std::env::var(home_var).ok();
            std::env::set_var(home_var, td.path());
            let p = td.path().to_path_buf();
            (
                HomeGuard {
                    old_home,
                    _td: td,
                },
                p,
            )
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            let home_var = if cfg!(target_os = "windows") {
                "USERPROFILE"
            } else {
                "HOME"
            };
            match &self.old_home {
                Some(v) => std::env::set_var(home_var, v),
                None => std::env::remove_var(home_var),
            }
        }
    }

    #[test]
    fn device_id_mints_on_first_call() {
        let (_guard, home) = HomeGuard::new();
        let id = device_id();
        assert!(!id.is_empty());
        assert_eq!(id.len(), 26, "ULID is 26 chars");

        // File should now exist.
        let file = home.join(".config").join("zestful").join("device.id");
        assert!(file.exists(), "device.id file should be created");
    }

    #[test]
    fn device_id_reads_existing() {
        let (_guard, home) = HomeGuard::new();
        let dir = home.join(".config").join("zestful");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("device.id"), "01JEXISTING1234567890ABCDE\n").unwrap();

        let id = device_id();
        assert_eq!(id, "01JEXISTING1234567890ABCDE");
    }

    #[test]
    fn device_id_stable_across_calls() {
        let (_guard, _home) = HomeGuard::new();
        let id1 = device_id();
        let id2 = device_id();
        assert_eq!(id1, id2);
    }
}
