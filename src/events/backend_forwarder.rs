//! Fire-and-forget HTTPS POST of accepted event envelopes to the Fly backend.
//!
//! Called from the daemon's `/events` handler after the local log line.
//! All errors are logged and swallowed — the daemon's local log remains the
//! source of truth regardless of backend availability.

use crate::config;
use once_cell::sync::Lazy;
use reqwest::Client;
use std::time::Duration;

const BACKEND_URL: &str = "https://zestful-api.fly.dev/v1/events";
const JWT_FILE: &str = "supabase.jwt";

static HTTP_CLIENT: Lazy<Option<Client>> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()
});

/// Spawn a background task that POSTs `envelopes` to the backend. Returns
/// immediately; the task runs to completion independently of the caller.
pub fn spawn_forward(envelopes: Vec<serde_json::Value>) {
    if envelopes.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let jwt = match read_jwt() {
            Some(j) => j,
            None => {
                crate::log::log("events", "no jwt on disk; skipping backend forward");
                return;
            }
        };
        let client = match &*HTTP_CLIENT {
            Some(c) => c,
            None => {
                crate::log::log("events", "http client unavailable; skipping backend forward");
                return;
            }
        };
        let body = serde_json::json!({ "events": envelopes });
        match client
            .post(BACKEND_URL)
            .bearer_auth(&jwt)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                let summary = if text.len() > 256 {
                    let end = text.char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= 256)
                        .last()
                        .unwrap_or(0);
                    format!("{}…", &text[..end])
                } else {
                    text
                };
                crate::log::log(
                    "events",
                    &format!("backend returned {}: {}", status, summary),
                );
            }
            Err(e) => {
                crate::log::log("events", &format!("backend forward failed: {}", e));
            }
        }
    });
}

/// Read the Supabase JWT the Mac app writes to `~/.config/zestful/supabase.jwt`.
/// Returns `None` if the file is missing, empty, or unreadable.
pub fn read_jwt() -> Option<String> {
    let path = config::config_dir().join(JWT_FILE);
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Redirect `$HOME` to a tempdir for the duration of a test.
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
            (HomeGuard { old_home, _td: td }, p)
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
    fn read_jwt_returns_none_when_file_missing() {
        let (_g, _home) = HomeGuard::new();
        assert_eq!(read_jwt(), None);
    }

    #[test]
    fn read_jwt_returns_trimmed_contents() {
        let (_g, home) = HomeGuard::new();
        let dir = home.join(".config").join("zestful");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("supabase.jwt"), "eyJhbGc.payload.sig\n\n").unwrap();
        assert_eq!(read_jwt().as_deref(), Some("eyJhbGc.payload.sig"));
    }

    #[test]
    fn read_jwt_returns_none_when_file_empty_or_whitespace() {
        let (_g, home) = HomeGuard::new();
        let dir = home.join(".config").join("zestful");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("supabase.jwt"), "   \n\n").unwrap();
        assert_eq!(read_jwt(), None);
    }

    #[test]
    fn spawn_forward_on_empty_slice_is_noop() {
        // Must not panic, must not spawn anything. Nothing to assert beyond
        // "it returns and doesn't blow up."
        spawn_forward(vec![]);
    }
}
