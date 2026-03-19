//! Configuration helpers for reading tokens, ports, focus context, and managing
//! the daemon lifecycle.
//!
//! Config files live in `~/.config/zestful/`:
//! - `local-token` — auth token shared with the Mac app
//! - `port` — override for the Mac app's HTTP port (default 21547)
//! - `focus-context` — saved focus state (`app=`, `window_id=`, `tab_id=`)
//! - `zestfuld.pid` — PID of the running focus daemon

use std::collections::HashMap;
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

/// Path to the focus context file.
pub fn focus_file() -> PathBuf {
    config_dir().join("focus-context")
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

/// Read focus context from the focus-context file (key=value format).
pub fn read_focus_context() -> HashMap<String, String> {
    let mut ctx = HashMap::new();
    if let Ok(contents) = fs::read_to_string(focus_file()) {
        for line in contents.lines() {
            if let Some((key, value)) = line.split_once('=') {
                ctx.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    ctx
}

/// Detect the actual terminal app, looking through multiplexers like tmux.
///
/// Checks terminal-specific env vars first (e.g. `ITERM_SESSION_ID` for iTerm2,
/// `KITTY_WINDOW_ID` for kitty), then falls back to `$TERM_PROGRAM`. Multiplexers
/// like tmux set `TERM_PROGRAM=tmux`, which can't be focused — we need the real
/// terminal wrapping it.
pub fn detect_terminal() -> Option<String> {
    // iTerm2 sets these even inside tmux
    if env::var("ITERM_SESSION_ID").is_ok() || env::var("ITERM_PROFILE").is_ok() {
        return Some("iTerm2".to_string());
    }
    // kitty sets KITTY_WINDOW_ID
    if env::var("KITTY_WINDOW_ID").is_ok() {
        return Some("kitty".to_string());
    }
    // WezTerm sets WEZTERM_EXECUTABLE
    if env::var("WEZTERM_EXECUTABLE").is_ok() {
        return Some("WezTerm".to_string());
    }
    // Fall back to TERM_PROGRAM, but skip multiplexers
    if let Ok(term) = env::var("TERM_PROGRAM") {
        let lower = term.to_lowercase();
        if lower != "tmux" && lower != "screen" {
            return Some(term);
        }
    }
    None
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
    use std::io::Write;
    use tempfile::TempDir;

    // Helper to set up a temp config dir
    fn with_temp_config<F: FnOnce(&std::path::Path)>(f: F) {
        let dir = TempDir::new().unwrap();
        f(dir.path());
    }

    #[test]
    fn test_read_focus_context_parses_key_value() {
        with_temp_config(|dir| {
            let file = dir.join("focus-context");
            let mut f = fs::File::create(&file).unwrap();
            writeln!(f, "app=kitty").unwrap();
            writeln!(f, "window_id=42").unwrap();
            writeln!(f, "tab_id=my-tab").unwrap();

            let contents = fs::read_to_string(&file).unwrap();
            let mut ctx = HashMap::new();
            for line in contents.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    ctx.insert(key.trim().to_string(), value.trim().to_string());
                }
            }

            assert_eq!(ctx.get("app").unwrap(), "kitty");
            assert_eq!(ctx.get("window_id").unwrap(), "42");
            assert_eq!(ctx.get("tab_id").unwrap(), "my-tab");
        });
    }

    #[test]
    fn test_read_focus_context_empty_file() {
        with_temp_config(|dir| {
            let file = dir.join("focus-context");
            fs::File::create(&file).unwrap();

            let contents = fs::read_to_string(&file).unwrap();
            let mut ctx = HashMap::new();
            for line in contents.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    ctx.insert(key.trim().to_string(), value.trim().to_string());
                }
            }

            assert!(ctx.is_empty());
        });
    }

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
        // PID 999999 almost certainly doesn't exist
        assert!(!libc_kill(999999));
    }

    #[test]
    fn test_libc_kill_current_process() {
        // Current process should be alive
        let pid = std::process::id() as i32;
        assert!(libc_kill(pid));
    }

    #[test]
    fn test_read_token_returns_some_or_none() {
        // Should not panic regardless of whether token file exists
        let _ = read_token();
    }

    #[test]
    fn test_read_port_returns_valid_port() {
        let port = read_port();
        assert!(port > 0);
    }

    #[test]
    fn test_read_focus_context_returns_map() {
        // Should not panic regardless of whether file exists
        let ctx = read_focus_context();
        // We can't assert specific values, but it should be a valid HashMap
        let _ = ctx.len();
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
    fn test_focus_file_path() {
        let path = focus_file();
        assert!(path.ends_with("focus-context"));
    }

    #[test]
    fn test_pid_file_path() {
        let path = pid_file();
        assert!(path.ends_with("zestfuld.pid"));
    }

    #[test]
    fn test_detect_terminal_returns_something() {
        // Should not panic; result depends on environment
        let _ = detect_terminal();
    }
}
