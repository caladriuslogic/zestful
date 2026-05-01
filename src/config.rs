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

/// Returns `~/.config/zestful/` on Unix-like systems or `%USERPROFILE%\.config\zestful\` on Windows.
pub fn config_dir() -> PathBuf {
    let home = if cfg!(target_os = "windows") {
        env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Default".to_string())
    } else {
        env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
    };
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

/// Ensure a `local-token` exists at `token_file()`. If absent or empty,
/// generate a 32-byte cryptographically-random token (hex-encoded), write
/// it with mode 0600, and return its path. No-op when a non-empty token
/// already exists — the macOS app, an admin, or a prior daemon run may
/// have written one.
///
/// Called from the daemon's `run_server` startup so Linux users get a
/// working token without manual `openssl rand` setup.
pub fn ensure_token() -> std::io::Result<PathBuf> {
    let path = token_file();
    if let Ok(s) = fs::read_to_string(&path) {
        if !s.trim().is_empty() {
            return Ok(path);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other,
                                         format!("getrandom: {}", e)))?;
    let token: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    fs::write(&path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// Read the auth token from config file, falling back to macOS UserDefaults.
pub fn read_token() -> Option<String> {
    if let Ok(v) = std::env::var("ZESTFUL_TOKEN_OVERRIDE") {
        return Some(v);
    }
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
                if let Ok(port) = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<u16>()
                {
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

    // Poll /health until the daemon is accepting connections, up to 2s.
    // A blind sleep is not enough when SQLite migration or port bind is slow.
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if daemon_is_healthy(DAEMON_PORT) {
            return;
        }
    }
}

fn daemon_is_healthy(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let Ok(mut stream) = TcpStream::connect(format!("127.0.0.1:{}", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    let req = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        port
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 32];
    matches!(stream.read(&mut buf), Ok(n) if n > 0 && buf.starts_with(b"HTTP/1.1 200"))
}

/// Check if a process is alive.
fn libc_kill(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: kill with signal 0 just checks process existence, no signal is sent.
        // pid is validated > 0 above.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(target_os = "windows")]
    {
        crate::workspace::win32::is_process_alive(pid as u32)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = pid;
        false
    }
}

/// Read the `scraper.enabled` setting. Defaults to `true` when unset
/// or unreadable. The scraper is on by default.
pub fn scraper_enabled() -> bool {
    read_settings_bool("scraper.enabled").unwrap_or(true)
}

/// Read a boolean from `~/.config/zestful/settings.json` at the given
/// dotted path (e.g. `"scraper.enabled"`). Returns `None` if the file
/// is missing, unparseable, or the key is absent.
fn read_settings_bool(dotted_path: &str) -> Option<bool> {
    let path = config_dir().join("settings.json");
    let bytes = std::fs::read(&path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let mut cur = &v;
    for segment in dotted_path.split('.') {
        cur = cur.get(segment)?;
    }
    cur.as_bool()
}

/// Read `scraper.extra_roots` from settings. Each entry is
/// `{"agent": "claude-code"|"codex", "path": "/some/path"}`.
/// Returns empty vec when the file/key is missing.
pub fn scraper_extra_roots() -> Vec<(String, String)> {
    let path = config_dir().join("settings.json");
    let bytes = match std::fs::read(&path) { Ok(b) => b, Err(_) => return vec![] };
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v, Err(_) => return vec![],
    };
    let arr = match v.pointer("/scraper/extra_roots").and_then(|x| x.as_array()) {
        Some(a) => a, None => return vec![],
    };
    arr.iter()
        .filter_map(|entry| {
            let agent = entry.get("agent")?.as_str()?.to_string();
            let path = entry.get("path")?.as_str()?.to_string();
            Some((agent, path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dir_uses_home() {
        let dir = config_dir();
        let path_str = dir.to_str().unwrap();
        assert!(path_str.contains(".config"));
        assert!(path_str.contains("zestful"));
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
    #[cfg(unix)]
    fn test_libc_kill_current_process() {
        let pid = std::process::id() as i32;
        assert!(libc_kill(pid));
    }

    #[test]
    fn test_read_token_returns_some_or_none() {
        let _ = read_token();
    }

    #[test]
    #[cfg(unix)]
    fn ensure_token_creates_a_token_when_absent() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::TempDir::new().unwrap();
        let prev = std::env::var("HOME").ok();
        // SAFETY: tests run --test-threads=1 (HomeGuard convention).
        unsafe { std::env::set_var("HOME", td.path()); }
        std::env::remove_var("ZESTFUL_TOKEN_OVERRIDE");

        let path = ensure_token().expect("create token");
        let s = std::fs::read_to_string(&path).expect("read token");
        assert_eq!(s.len(), 64, "expected 64 hex chars (32 bytes), got {}", s.len());
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()),
                "expected hex chars only: {:?}", s);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected mode 0600, got {:o}", mode);

        if let Some(p) = prev { unsafe { std::env::set_var("HOME", p); } }
        else { unsafe { std::env::remove_var("HOME"); } }
    }

    #[test]
    #[cfg(unix)]
    fn ensure_token_does_not_overwrite_existing() {
        let td = tempfile::TempDir::new().unwrap();
        let prev = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", td.path()); }
        std::env::remove_var("ZESTFUL_TOKEN_OVERRIDE");

        let dir = config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("local-token"), "preexisting-secret").unwrap();
        let path = ensure_token().expect("ok");
        let s = std::fs::read_to_string(&path).unwrap();
        assert_eq!(s, "preexisting-secret", "must not overwrite existing token");

        if let Some(p) = prev { unsafe { std::env::set_var("HOME", p); } }
        else { unsafe { std::env::remove_var("HOME"); } }
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
        let path_str = path.to_str().unwrap();
        assert!(path_str.contains(".config"));
        assert!(path_str.contains("zestful"));
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
