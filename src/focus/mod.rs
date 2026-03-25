//! Terminal focus handlers.
//!
//! Each submodule implements focus switching for a specific terminal emulator.
//! The [`handle_focus`] function dispatches to the correct handler based on the
//! app name. All inputs are validated against an allowlist before use.

#[cfg(target_os = "macos")]
pub mod iterm2;
pub mod kitty;
pub mod terminal;
pub mod wezterm;

use anyhow::{bail, Result};

/// Parsed terminal URI components.
pub struct ParsedTerminalUri {
    pub app: String,
    pub window_id: Option<String>,
    pub tab_id: Option<String>,
}

/// Parse a `terminal://` URI into app name and IDs for focus dispatch.
///
/// URI format: `terminal://iterm2/window:1229/tab:3/tmux:main/window:1/pane:0`
/// Extracts the terminal emulator name and its window/tab IDs (before any
/// multiplexer segments).
pub fn parse_terminal_uri(uri: &str) -> Option<ParsedTerminalUri> {
    let rest = uri.strip_prefix("terminal://")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let raw_app = parts.first()?;
    if raw_app.is_empty() {
        return None;
    }

    let mut window_id = None;
    let mut tab_id = None;

    for part in &parts[1..] {
        // Stop at multiplexer segments — those are inside the terminal
        if part.starts_with("tmux:")
            || part.starts_with("zellij:")
            || part.starts_with("shelldon:")
        {
            break;
        }
        if let Some(id) = part.strip_prefix("window:") {
            window_id = Some(id.to_string());
        } else if let Some(id) = part.strip_prefix("tab:") {
            tab_id = Some(id.to_string());
        }
    }

    let app = match *raw_app {
        "iterm2" => "iTerm2".to_string(),
        "kitty" => "kitty".to_string(),
        "wezterm" => "WezTerm".to_string(),
        "terminal" | "apple_terminal" => "Terminal".to_string(),
        other => other.to_string(),
    };

    Some(ParsedTerminalUri {
        app,
        window_id,
        tab_id,
    })
}

/// Validate that a focus identifier (app, window_id, tab_id) contains only
/// safe characters. Prevents command injection via osascript or CLI args.
pub fn validate_focus_id(value: &str, field: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{} must not be empty", field);
    }
    // Allow alphanumeric, spaces, dashes, underscores, dots, colons, slashes
    // (covers app names like "iTerm2", paths like "/dev/ttys001", IDs like "tab:123")
    if !value
        .chars()
        .all(|c| c.is_alphanumeric() || " -_.:/@".contains(c))
    {
        bail!(
            "{} contains invalid characters: only alphanumeric, spaces, and -_.:/@  are allowed",
            field
        );
    }
    Ok(())
}

/// Escape a string for safe embedding in AppleScript double-quoted strings.
#[cfg(target_os = "macos")]
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Dispatch focus to the appropriate terminal handler.
pub async fn handle_focus(app: &str, window_id: Option<&str>, tab_id: Option<&str>) -> Result<()> {
    validate_focus_id(app, "app")?;
    if let Some(wid) = window_id {
        validate_focus_id(wid, "window_id")?;
    }
    if let Some(tid) = tab_id {
        validate_focus_id(tid, "tab_id")?;
    }

    let lower = app.to_lowercase();

    if lower.contains("kitty") {
        kitty::focus(window_id, tab_id).await
    } else if lower.contains("iterm") {
        #[cfg(target_os = "macos")]
        {
            iterm2::focus(window_id, tab_id).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            activate_generic(app).await
        }
    } else if lower.contains("wezterm") {
        wezterm::focus(tab_id).await
    } else if lower.contains("terminal") {
        terminal::focus(tab_id).await
    } else {
        activate_generic(app).await
    }
}

/// Generic app activation via osascript (macOS) or no-op.
pub async fn activate_generic(app: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let app = app.to_string();
        tokio::task::spawn_blocking(move || {
            let escaped = escape_applescript(&app);
            let _ = std::process::Command::new("osascript")
                .args(["-e", &format!("tell application \"{}\" to activate", escaped)])
                .output();
        })
        .await?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
    Ok(())
}

/// Activate an app by name via osascript. Used by focus handlers.
#[cfg(target_os = "macos")]
pub fn activate_app_sync(app: &str) {
    let escaped = escape_applescript(app);
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"{}\" to activate", escaped)])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_focus_dispatches_kitty() {
        // Should not panic even though kitty isn't running
        let result = handle_focus("kitty", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_wezterm() {
        let result = handle_focus("WezTerm", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_terminal() {
        let result = handle_focus("Terminal", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_generic() {
        let result = handle_focus("SomeRandomApp", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_case_insensitive() {
        // "KITTY" should route to kitty handler
        let result = handle_focus("KITTY", None, None).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_terminal_uri_iterm2() {
        let parsed = parse_terminal_uri("terminal://iterm2/window:1229/tab:3").unwrap();
        assert_eq!(parsed.app, "iTerm2");
        assert_eq!(parsed.window_id.as_deref(), Some("1229"));
        assert_eq!(parsed.tab_id.as_deref(), Some("3"));
    }

    #[test]
    fn test_parse_terminal_uri_with_tmux() {
        let parsed =
            parse_terminal_uri("terminal://iterm2/window:1229/tab:3/tmux:main/window:1/pane:0")
                .unwrap();
        assert_eq!(parsed.app, "iTerm2");
        // Should capture terminal-level window/tab, not tmux-level
        assert_eq!(parsed.window_id.as_deref(), Some("1229"));
        assert_eq!(parsed.tab_id.as_deref(), Some("3"));
    }

    #[test]
    fn test_parse_terminal_uri_kitty() {
        let parsed = parse_terminal_uri("terminal://kitty/window:42/tab:7").unwrap();
        assert_eq!(parsed.app, "kitty");
        assert_eq!(parsed.window_id.as_deref(), Some("42"));
        assert_eq!(parsed.tab_id.as_deref(), Some("7"));
    }

    #[test]
    fn test_parse_terminal_uri_no_ids() {
        let parsed = parse_terminal_uri("terminal://wezterm").unwrap();
        assert_eq!(parsed.app, "WezTerm");
        assert!(parsed.window_id.is_none());
        assert!(parsed.tab_id.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_invalid() {
        assert!(parse_terminal_uri("not-a-uri").is_none());
        assert!(parse_terminal_uri("terminal://").is_none());
        assert!(parse_terminal_uri("http://iterm2/window:1").is_none());
    }

    #[test]
    fn test_parse_terminal_uri_apple_terminal() {
        let parsed = parse_terminal_uri("terminal://apple_terminal/window:1").unwrap();
        assert_eq!(parsed.app, "Terminal");
    }

    #[test]
    fn test_validate_focus_id_accepts_valid() {
        assert!(validate_focus_id("kitty", "app").is_ok());
        assert!(validate_focus_id("iTerm2", "app").is_ok());
        assert!(validate_focus_id("Terminal.app", "app").is_ok());
        assert!(validate_focus_id("/dev/ttys001", "tab_id").is_ok());
        assert!(validate_focus_id("my-tab:123", "tab_id").is_ok());
        assert!(validate_focus_id("42", "window_id").is_ok());
    }

    #[test]
    fn test_validate_focus_id_rejects_injection() {
        // AppleScript injection attempts
        assert!(validate_focus_id("Finder\"; display dialog \"pwned", "app").is_err());
        assert!(validate_focus_id("app\nmalicious", "app").is_err());
        assert!(validate_focus_id("", "app").is_err());
        assert!(validate_focus_id("tab$(whoami)", "tab_id").is_err());
        assert!(validate_focus_id("tab`id`", "tab_id").is_err());
    }

    #[tokio::test]
    async fn test_handle_focus_rejects_invalid_app() {
        let result = handle_focus("bad\"app", None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_focus_rejects_invalid_tab_id() {
        let result = handle_focus("kitty", None, Some("tab$(whoami)")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_focus_rejects_invalid_window_id() {
        let result = handle_focus("kitty", Some("win`id`"), None).await;
        assert!(result.is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_escape_applescript() {
        assert_eq!(escape_applescript("hello"), "hello");
        assert_eq!(escape_applescript(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_applescript(r"path\to"), r"path\\to");
    }
}
