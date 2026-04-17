//! URI parsing and validation for terminal focus dispatch.

use anyhow::{bail, Result};

/// Parsed shelldon multiplexer info from the URI.
pub struct ShelldonInfo {
    pub session_id: String,
    pub tab_id: Option<String>,
}

/// Parsed tmux multiplexer info from the URI.
pub struct TmuxInfo {
    pub session: String,
    pub window: Option<String>,
    pub pane: Option<String>,
}

/// Parsed terminal URI components.
pub struct ParsedTerminalUri {
    pub app: String,
    pub window_id: Option<String>,
    pub tab_id: Option<String>,
    pub shelldon: Option<ShelldonInfo>,
    pub tmux: Option<TmuxInfo>,
}

/// Parse a `workspace://` or `terminal://` URI into app name and IDs for focus dispatch.
///
/// URI format: `workspace://iterm2/window:1229/tab:3/shelldon:session-id/tab:0`
pub fn parse_terminal_uri(uri: &str) -> Option<ParsedTerminalUri> {
    let rest = uri
        .strip_prefix("workspace://")
        .or_else(|| uri.strip_prefix("terminal://"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    let raw_app = parts.first()?;
    if raw_app.is_empty() {
        return None;
    }

    let mut window_id = None;
    let mut tab_id = None;
    let mut shelldon = None;

    let mut in_shelldon = false;
    let mut shelldon_session_id = String::new();
    let mut shelldon_tab_id = None;

    let mut in_tmux = false;
    let mut tmux_session = String::new();
    let mut tmux_window = None;
    let mut tmux_pane = None;

    for part in &parts[1..] {
        if in_shelldon {
            if let Some(id) = part.strip_prefix("tab:") {
                shelldon_tab_id = Some(id.to_string());
            }
            continue;
        }

        if in_tmux {
            if let Some(id) = part.strip_prefix("window:") {
                tmux_window = Some(id.to_string());
            } else if let Some(id) = part.strip_prefix("pane:") {
                tmux_pane = Some(id.to_string());
            }
            continue;
        }

        if let Some(session) = part.strip_prefix("shelldon:") {
            in_shelldon = true;
            shelldon_session_id = session.to_string();
            continue;
        }

        if let Some(session) = part.strip_prefix("tmux:") {
            in_tmux = true;
            tmux_session = session.to_string();
            continue;
        }

        // Stop at other multiplexer segments
        if part.starts_with("zellij:") {
            break;
        }

        if let Some(id) = part.strip_prefix("window:") {
            window_id = Some(id.to_string());
        } else if let Some(id) = part.strip_prefix("tab:") {
            tab_id = Some(id.to_string());
        }
    }

    if in_shelldon {
        shelldon = Some(ShelldonInfo {
            session_id: shelldon_session_id,
            tab_id: shelldon_tab_id,
        });
    }

    let tmux = if in_tmux {
        Some(TmuxInfo {
            session: tmux_session,
            window: tmux_window,
            pane: tmux_pane,
        })
    } else {
        None
    };

    let app = match *raw_app {
        "cmd" => "Cmd".to_string(),
        "iterm2" => "iTerm2".to_string(),
        "kitty" => "kitty".to_string(),
        "powershell" => "PowerShell".to_string(),
        "wezterm" => "WezTerm".to_string(),
        "terminal" | "apple_terminal" => "Terminal".to_string(),
        "windows-terminal" => "Windows Terminal".to_string(),
        "chrome" => "Google Chrome".to_string(),
        other => other.to_string(),
    };

    Some(ParsedTerminalUri {
        app,
        window_id,
        tab_id,
        shelldon,
        tmux,
    })
}

/// Validate that a focus identifier contains only safe characters.
/// Prevents command injection via osascript or CLI args.
pub fn validate_focus_id(value: &str, field: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{} must not be empty", field);
    }
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
pub fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Activate an app by name via osascript. Used by focus handlers.
#[cfg(target_os = "macos")]
pub fn activate_app_sync(app: &str) {
    let escaped = escape_applescript(app);
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"{}\" to activate", escaped)])
        .output();
}

/// Generic app activation via osascript (macOS) or no-op.
pub async fn activate_generic(app: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let app = app.to_string();
        tokio::task::spawn_blocking(move || {
            activate_app_sync(&app);
        })
        .await?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_terminal_uri_iterm2() {
        let parsed = parse_terminal_uri("workspace://iterm2/window:1229/tab:3").unwrap();
        assert_eq!(parsed.app, "iTerm2");
        assert_eq!(parsed.window_id.as_deref(), Some("1229"));
        assert_eq!(parsed.tab_id.as_deref(), Some("3"));
    }

    #[test]
    fn test_parse_terminal_uri_with_tmux() {
        let parsed =
            parse_terminal_uri("workspace://iterm2/window:1229/tab:3/tmux:main/window:1/pane:0")
                .unwrap();
        assert_eq!(parsed.app, "iTerm2");
        assert_eq!(parsed.window_id.as_deref(), Some("1229"));
        assert_eq!(parsed.tab_id.as_deref(), Some("3"));
        assert!(parsed.shelldon.is_none());
        let tmux = parsed.tmux.as_ref().unwrap();
        assert_eq!(tmux.session, "main");
        assert_eq!(tmux.window.as_deref(), Some("1"));
        assert_eq!(tmux.pane.as_deref(), Some("0"));
    }

    #[test]
    fn test_parse_terminal_uri_tmux_session_only() {
        let parsed =
            parse_terminal_uri("workspace://iterm2/window:1/tab:1/tmux:dev")
                .unwrap();
        let tmux = parsed.tmux.as_ref().unwrap();
        assert_eq!(tmux.session, "dev");
        assert!(tmux.window.is_none());
        assert!(tmux.pane.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_tmux_window_no_pane() {
        let parsed =
            parse_terminal_uri("workspace://iterm2/window:1/tab:1/tmux:main/window:2")
                .unwrap();
        let tmux = parsed.tmux.as_ref().unwrap();
        assert_eq!(tmux.session, "main");
        assert_eq!(tmux.window.as_deref(), Some("2"));
        assert!(tmux.pane.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_no_tmux() {
        let parsed = parse_terminal_uri("workspace://kitty/window:1/tab:2").unwrap();
        assert!(parsed.tmux.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_with_shelldon() {
        let parsed = parse_terminal_uri(
            "workspace://iterm2/window:26411/tab:1/shelldon:shelldon-28647-56756/tab:0",
        )
        .unwrap();
        assert_eq!(parsed.app, "iTerm2");
        assert_eq!(parsed.window_id.as_deref(), Some("26411"));
        assert_eq!(parsed.tab_id.as_deref(), Some("1"));
        let shelldon = parsed.shelldon.as_ref().unwrap();
        assert_eq!(shelldon.session_id, "shelldon-28647-56756");
        assert_eq!(shelldon.tab_id.as_deref(), Some("0"));
    }

    #[test]
    fn test_parse_terminal_uri_shelldon_no_tab() {
        let parsed = parse_terminal_uri(
            "workspace://iterm2/window:26411/tab:1/shelldon:shelldon-28647-56756",
        )
        .unwrap();
        let shelldon = parsed.shelldon.as_ref().unwrap();
        assert_eq!(shelldon.session_id, "shelldon-28647-56756");
        assert!(shelldon.tab_id.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_kitty() {
        let parsed = parse_terminal_uri("workspace://kitty/window:42/tab:7").unwrap();
        assert_eq!(parsed.app, "kitty");
        assert_eq!(parsed.window_id.as_deref(), Some("42"));
        assert_eq!(parsed.tab_id.as_deref(), Some("7"));
    }

    #[test]
    fn test_parse_terminal_uri_no_ids() {
        let parsed = parse_terminal_uri("workspace://wezterm").unwrap();
        assert_eq!(parsed.app, "WezTerm");
        assert!(parsed.window_id.is_none());
        assert!(parsed.tab_id.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_legacy_scheme() {
        let parsed = parse_terminal_uri("terminal://kitty/window:1/tab:2").unwrap();
        assert_eq!(parsed.app, "kitty");
        assert_eq!(parsed.tab_id.as_deref(), Some("2"));
    }

    #[test]
    fn test_parse_terminal_uri_invalid() {
        assert!(parse_terminal_uri("not-a-uri").is_none());
        assert!(parse_terminal_uri("workspace://").is_none());
        assert!(parse_terminal_uri("http://iterm2/window:1").is_none());
    }

    #[test]
    fn test_parse_terminal_uri_cmd() {
        let parsed = parse_terminal_uri("workspace://cmd/window:5678").unwrap();
        assert_eq!(parsed.app, "Cmd");
        assert_eq!(parsed.window_id.as_deref(), Some("5678"));
        assert!(parsed.tab_id.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_powershell() {
        let parsed = parse_terminal_uri("workspace://powershell/window:5678").unwrap();
        assert_eq!(parsed.app, "PowerShell");
        assert_eq!(parsed.window_id.as_deref(), Some("5678"));
        assert!(parsed.tab_id.is_none());
    }

    #[test]
    fn test_parse_terminal_uri_apple_terminal() {
        let parsed = parse_terminal_uri("workspace://apple_terminal/window:1").unwrap();
        assert_eq!(parsed.app, "Terminal");
    }

    #[test]
    fn test_parse_terminal_uri_windows_terminal() {
        let parsed = parse_terminal_uri("workspace://windows-terminal/window:131072/tab:2").unwrap();
        assert_eq!(parsed.app, "Windows Terminal");
        assert_eq!(parsed.window_id.as_deref(), Some("131072"));
        assert_eq!(parsed.tab_id.as_deref(), Some("2"));
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
        assert!(validate_focus_id("Finder\"; display dialog \"pwned", "app").is_err());
        assert!(validate_focus_id("app\nmalicious", "app").is_err());
        assert!(validate_focus_id("", "app").is_err());
        assert!(validate_focus_id("tab$(whoami)", "tab_id").is_err());
        assert!(validate_focus_id("tab`id`", "tab_id").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_escape_applescript() {
        assert_eq!(escape_applescript("hello"), "hello");
        assert_eq!(escape_applescript(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_applescript(r"path\to"), r"path\\to");
    }
}
