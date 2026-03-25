//! Configuration helpers for reading tokens, ports, and managing the daemon
//! lifecycle.
//!
//! Config files live in `~/.config/zestful/`:
//! - `local-token` — auth token shared with the Mac app
//! - `port` — override for the Mac app's HTTP port (default 21547)
//! - `zestfuld.pid` — PID of the running focus daemon

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Default port for the Zestful Mac app's HTTP server.
const DEFAULT_PORT: u16 = 21547;

/// Port the focus daemon listens on.
const DAEMON_PORT: u16 = 21548;

/// Returns `~/.config/zestful/`.
pub fn config_dir() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config").join("zestful")
}

/// Path to the auth token file.
pub fn token_file() -> PathBuf {
    config_dir().join("local-token")
}

/// Path to the port override file.
pub fn port_file() -> PathBuf {
    config_dir().join("port")
}

/// Path to the daemon PID file.
pub fn pid_file() -> PathBuf {
    config_dir().join("zestfuld.pid")
}

/// Returns the daemon's listening port (21548).
pub fn daemon_port() -> u16 {
    DAEMON_PORT
}

/// Read the auth token from config file, falling back to macOS UserDefaults.
pub fn read_token() -> Option<String> {
    // Try file first
    if let Ok(token) = fs::read_to_string(token_file()) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }

    // Fallback: macOS UserDefaults
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("defaults")
            .args(["read", "com.caladriuslogic.zestful", "localServerToken"])
            .output()
        {
            if output.status.success() {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !token.is_empty() {
                    return Some(token);
                }
            }
        }
    }

    None
}

/// Read the port from config file, falling back to macOS UserDefaults, then default.
pub fn read_port() -> u16 {
    // Try file first
    if let Ok(port_str) = fs::read_to_string(port_file()) {
        if let Ok(port) = port_str.trim().parse::<u16>() {
            return port;
        }
    }

    // Fallback: macOS UserDefaults
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("defaults")
            .args(["read", "com.caladriuslogic.zestful", "localServerPort"])
            .output()
        {
            if output.status.success() {
                if let Ok(port) = String::from_utf8_lossy(&output.stdout).trim().parse::<u16>() {
                    return port;
                }
            }
        }
    }

    DEFAULT_PORT
}

/// Read the saved terminal URI (written by `zestful ssh` for remote sessions).
pub fn read_terminal_uri() -> Option<String> {
    let path = config_dir().join("terminal-uri");
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Ensure the daemon is running. If not, spawn `zestful daemon` detached.
pub fn ensure_daemon() {
    // Check PID file
    if let Ok(pid_str) = fs::read_to_string(pid_file()) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Check if process is alive with kill -0
            if libc_kill(pid) {
                return;
            }
        }
    }

    // Spawn daemon using our own binary
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("zestful"));
    let _ = Command::new(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    // Brief wait for daemon startup
    std::thread::sleep(std::time::Duration::from_millis(300));
}

/// Check if a process is alive using kill(pid, 0).
fn libc_kill(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 just checks process existence, no signal is sent.
    // pid is validated > 0 above, so we won't send to process group 0 or negative groups.
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dir_uses_home() {
        let dir = config_dir();
        assert!(dir.to_str().unwrap().contains(".config/zestful"));
    }

    #[test]
    fn test_default_port() {
        assert_eq!(DEFAULT_PORT, 21547);
    }

    #[test]
    fn test_daemon_port() {
        assert_eq!(daemon_port(), 21548);
    }

    #[test]
    fn test_libc_kill_rejects_zero_pid() {
        assert!(!libc_kill(0));
    }

    #[test]
    fn test_libc_kill_rejects_negative_pid() {
        assert!(!libc_kill(-1));
        assert!(!libc_kill(-999));
    }

    #[test]
    fn test_libc_kill_nonexistent_pid() {
        assert!(!libc_kill(999999));
    }

    #[test]
    fn test_libc_kill_current_process() {
        let pid = std::process::id() as i32;
        assert!(libc_kill(pid));
    }

    #[test]
    fn test_read_token_returns_some_or_none() {
        let _ = read_token();
    }

    #[test]
    fn test_read_port_returns_valid_port() {
        let port = read_port();
        assert!(port > 0);
    }

    #[test]
    fn test_token_file_path() {
        let path = token_file();
        assert!(path.ends_with("local-token"));
        assert!(path.to_str().unwrap().contains(".config/zestful"));
    }

    #[test]
    fn test_port_file_path() {
        let path = port_file();
        assert!(path.ends_with("port"));
    }

    #[test]
    fn test_pid_file_path() {
        let path = pid_file();
        assert!(path.ends_with("zestfuld.pid"));
    }
}
