//! Fire-and-forget HTTPS POST of accepted event envelopes to the Fly backend.
//!
//! Called from the daemon's `/events` handler after the local log line.
//! All errors are logged and swallowed — the daemon's local log remains the
//! source of truth regardless of backend availability.

use crate::config;
use once_cell::sync::Lazy;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

const BACKEND_URL: &str = "https://zestful-api.fly.dev/v1/events";
const JWT_FILE: &str = "supabase.jwt";

/// Minimum interval between log lines for the same "reason" string. Prevents
/// spam when the Mac app is quit overnight (continuous 401s or missing JWT)
/// from filling the daemon log with identical lines every time a hook fires.
const LOG_RATE_LIMIT_MS: u64 = 5 * 60 * 1000; // 5 minutes

static HTTP_CLIENT: Lazy<Option<Client>> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()
});

/// Per-reason timestamp of last emitted log line. Keeps the map bounded at
/// one entry per distinct reason string (handful in practice).
static LAST_LOG_AT_MS: Lazy<Mutex<HashMap<String, u64>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Returns true if the caller should emit a log line for `reason` now, false
/// if the last log for that reason was within `LOG_RATE_LIMIT_MS`. Records
/// "now" as the last-emit time only when returning true.
pub fn should_log_reason(reason: &str) -> bool {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut map = match LAST_LOG_AT_MS.lock() {
        Ok(m) => m,
        // Poisoned mutex — fail open (still emit the log).
        Err(poisoned) => poisoned.into_inner(),
    };
    let last = map.get(reason).copied().unwrap_or(0);
    if now_ms.saturating_sub(last) >= LOG_RATE_LIMIT_MS {
        map.insert(reason.to_string(), now_ms);
        true
    } else {
        false
    }
}

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
                if should_log_reason("no-jwt") {
                    crate::log::log(
                        "events",
                        "no jwt on disk; skipping backend forward (further logs throttled for 5m)",
                    );
                }
                return;
            }
        };
        let client = match &*HTTP_CLIENT {
            Some(c) => c,
            None => {
                if should_log_reason("no-http-client") {
                    crate::log::log("events", "http client unavailable; skipping backend forward");
                }
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
                let reason = format!("backend-status-{}", status.as_u16());
                if should_log_reason(&reason) {
                    let text = resp.text().await.unwrap_or_default();
                    let summary = if text.len() > 256 {
                        let end = text
                            .char_indices()
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
            }
            Err(e) => {
                // Key on error kind (not the full message) so transient different
                // error messages from the same class don't defeat the throttle.
                let reason = format!("forward-err-{:?}", e.status().map(|s| s.as_u16()));
                if should_log_reason(&reason) {
                    crate::log::log("events", &format!("backend forward failed: {}", e));
                }
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
            // SAFETY: tests run single-threaded via --test-threads=1; no other
            // thread is reading env vars during this mutation.
            unsafe { std::env::set_var(home_var, td.path()); }
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
            // SAFETY: tests run single-threaded via --test-threads=1.
            unsafe {
                match &self.old_home {
                    Some(v) => std::env::set_var(home_var, v),
                    None => std::env::remove_var(home_var),
                }
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

    // Rate-limiter tests use distinct reason strings per test so that
    // process-global state doesn't cause cross-test flakiness under
    // --test-threads=1 (tests run sequentially but the map persists across).

    #[test]
    fn should_log_reason_first_call_emits() {
        assert!(should_log_reason("rl-test-first"));
    }

    #[test]
    fn should_log_reason_immediate_repeat_is_throttled() {
        assert!(should_log_reason("rl-test-repeat"));
        assert!(!should_log_reason("rl-test-repeat"));
        assert!(!should_log_reason("rl-test-repeat"));
    }

    #[test]
    fn should_log_reason_buckets_are_independent() {
        assert!(should_log_reason("rl-test-bucket-a"));
        // Throttled on bucket-a
        assert!(!should_log_reason("rl-test-bucket-a"));
        // But a distinct reason gets its own window
        assert!(should_log_reason("rl-test-bucket-b"));
    }
}
