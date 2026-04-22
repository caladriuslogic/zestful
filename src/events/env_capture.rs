//! Whitelist-only capture of well-known environment variables, surfaced on
//! event context as `env_vars_observed`. Collect-don't-interpret.

use std::collections::HashMap;

const WHITELIST: &[&str] = &[
    // Agent project roots
    "CLAUDE_PROJECT_DIR", "GEMINI_PROJECT_DIR", "CLAUDECODE",
    // Terminal identity
    "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
    "ITERM_PROFILE", "ITERM_SESSION_ID", "WT_SESSION",
    // Multiplexer / remote
    "TMUX", "TMUX_PANE", "SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT",
];

const MAX_VALUE_BYTES: usize = 4096;
const TRUNCATION_SUFFIX: &str = "…<truncated>";

/// Reads whitelisted env vars from the current process env. Returns None
/// if no whitelisted vars are set (including empty-string), or Some(map)
/// otherwise. Values over MAX_VALUE_BYTES are truncated with a suffix.
pub fn capture() -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    for &name in WHITELIST {
        if let Ok(value) = std::env::var(name) {
            if value.is_empty() { continue; }
            out.insert(name.to_string(), truncate(value));
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Truncate at a UTF-8 char boundary to avoid panicking on mid-codepoint
/// splits. Returns the original value if within limit.
fn truncate(value: String) -> String {
    if value.len() <= MAX_VALUE_BYTES { return value; }
    let mut end = MAX_VALUE_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], TRUNCATION_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn unset_all_whitelist() {
        for &name in WHITELIST {
            unsafe { std::env::remove_var(name); }
        }
    }

    fn save_whitelist() -> Vec<(&'static str, Option<String>)> {
        WHITELIST.iter().map(|&name| (name, std::env::var(name).ok())).collect()
    }

    fn restore_whitelist(saved: Vec<(&'static str, Option<String>)>) {
        for (name, prior) in saved {
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(name, v),
                    None    => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn capture_returns_none_when_no_whitelisted_vars_set() {
        let saved = save_whitelist();
        unset_all_whitelist();
        assert_eq!(capture(), None);
        restore_whitelist(saved);
    }

    #[test]
    #[serial]
    fn capture_returns_subset_of_whitelist() {
        let saved = save_whitelist();
        unset_all_whitelist();
        unsafe {
            std::env::set_var("CLAUDE_PROJECT_DIR", "/x");
            std::env::set_var("TMUX", "/t");
        }
        let got = capture().expect("expected Some(map)");
        assert_eq!(got.len(), 2);
        assert_eq!(got.get("CLAUDE_PROJECT_DIR").map(String::as_str), Some("/x"));
        assert_eq!(got.get("TMUX").map(String::as_str), Some("/t"));
        restore_whitelist(saved);
    }

    #[test]
    #[serial]
    fn capture_skips_empty_values() {
        let saved = save_whitelist();
        unset_all_whitelist();
        unsafe { std::env::set_var("TMUX", ""); }
        assert_eq!(capture(), None);
        restore_whitelist(saved);
    }

    #[test]
    #[serial]
    fn capture_truncates_oversized_values() {
        let saved = save_whitelist();
        unset_all_whitelist();
        let big = "x".repeat(10_000);
        unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", &big); }
        let got = capture().expect("expected Some(map)");
        let v = got.get("CLAUDE_PROJECT_DIR").expect("expected key").clone();
        assert!(v.ends_with(TRUNCATION_SUFFIX), "value should end with truncation suffix");
        assert!(v.len() <= MAX_VALUE_BYTES + TRUNCATION_SUFFIX.len(),
                "truncated value should be <= cap + suffix bytes, got {}", v.len());
        restore_whitelist(saved);
    }

    #[test]
    fn truncate_respects_utf8_boundaries() {
        // Build a value where byte MAX_VALUE_BYTES falls mid-codepoint.
        // 4095 ASCII bytes + "日本語" (9 UTF-8 bytes) = 4104 bytes total.
        // The 4096th byte is inside the first Japanese character.
        let mut s = String::with_capacity(4200);
        for _ in 0..4095 { s.push('x'); }
        s.push_str("日本語");
        let out = truncate(s);
        assert!(out.ends_with(TRUNCATION_SUFFIX));
        assert!(out.len() <= MAX_VALUE_BYTES + TRUNCATION_SUFFIX.len());
    }

    #[test]
    fn whitelist_matches_spec() {
        // Canonical list in:
        //   zestful-internal/docs/superpowers/specs/2026-04-22-env-vars-observed-design.md
        assert_eq!(WHITELIST, &[
            "CLAUDE_PROJECT_DIR", "GEMINI_PROJECT_DIR", "CLAUDECODE",
            "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
            "ITERM_PROFILE", "ITERM_SESSION_ID", "WT_SESSION",
            "TMUX", "TMUX_PANE", "SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT",
        ]);
    }
}
