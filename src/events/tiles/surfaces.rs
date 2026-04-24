//! Per-surface helpers: tmux/CLI tokens, browser URL → conversation
//! slug, VS Code window tokens, human-readable labels.

/// Build the CLI surface token. If subapplication kind is "tmux",
/// returns "tmux:<session>/pane:<pane>" using the subapplication
/// session/pane fields. Else returns the application_instance value
/// (e.g. "window:ttys000/tab:1"). None if neither is available.
pub fn cli_surface_token(
    subapp_kind: Option<&str>,
    subapp_session: Option<&str>,
    subapp_pane: Option<&str>,
    application_instance: Option<&str>,
) -> Option<String> {
    if subapp_kind == Some("tmux") {
        if let (Some(s), Some(p)) = (subapp_session, subapp_pane) {
            return Some(format!("tmux:{}/pane:{}", s, p));
        }
    }
    application_instance.map(String::from)
}

/// Parse a browser URL to extract the conversation slug.
/// claude.ai/chats/<uuid> → Some("<uuid>")
/// chatgpt.com/c/<uuid> or chat.openai.com/c/<uuid> → Some("<uuid>")
/// gemini.google.com/app/<chatid> → Some("<chatid>")
/// Anything else (homepage, unknown site, malformed) → None.
pub fn browser_conversation_slug(url: &str) -> Option<String> {
    let (host, path) = parse_host_and_path(url)?;
    let path_only = path.split(|c| c == '?' || c == '#').next().unwrap_or("");
    match host.as_str() {
        "claude.ai" => path_only.strip_prefix("/chats/").map(|s| s.trim_end_matches('/').to_string()),
        "chatgpt.com" | "chat.openai.com" => path_only.strip_prefix("/c/").map(|s| s.trim_end_matches('/').to_string()),
        "gemini.google.com" => path_only.strip_prefix("/app/").map(|s| s.trim_end_matches('/').to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

/// Derive the agent slug from a browser URL host. claude.ai →
/// claude-web; chatgpt.com / chat.openai.com → chatgpt-web;
/// gemini.google.com → gemini-web. None for unknown hosts.
pub fn browser_agent_for_url(url: &str) -> Option<String> {
    let (host, _) = parse_host_and_path(url)?;
    match host.as_str() {
        "claude.ai" => Some("claude-web".to_string()),
        "chatgpt.com" | "chat.openai.com" => Some("chatgpt-web".to_string()),
        "gemini.google.com" => Some("gemini-web".to_string()),
        _ => None,
    }
}

/// VS Code surface token from window pid (which lives in
/// application_instance for vscode events per the events spec).
pub fn vscode_surface_token(window_pid: &str) -> String {
    format!("vscode-window:{}", window_pid)
}

/// Human-display label for a surface. Examples:
/// - cli + "tmux:zestful/pane:%0" → "tmux zestful → pane %0"
/// - cli + "window:ttys000/tab:1" → "iTerm2 window ttys000 / tab 1"
/// - browser + "abc12345..." → "conversation abc12345…"
/// - vscode + "vscode-window:1234" → "VS Code window 1234"
/// Generic fallback if nothing matches: just return the token.
pub fn surface_label(surface_kind: &str, surface_token: &str) -> String {
    match surface_kind {
        "cli" => {
            if let Some(rest) = surface_token.strip_prefix("tmux:") {
                if let Some((session, pane_part)) = rest.split_once("/pane:") {
                    return format!("tmux {} \u{2192} pane {}", session, pane_part);
                }
            }
            if let Some(rest) = surface_token.strip_prefix("window:") {
                if let Some((win, tab_part)) = rest.split_once("/tab:") {
                    return format!("iTerm2 window {} / tab {}", win, tab_part);
                }
            }
            surface_token.to_string()
        }
        "browser" => {
            if surface_token.is_empty() {
                return "conversation".to_string();
            }
            let truncated = if surface_token.chars().count() > 8 {
                let cut: String = surface_token.chars().take(8).collect();
                format!("{}\u{2026}", cut)
            } else {
                surface_token.to_string()
            };
            format!("conversation {}", truncated)
        }
        "vscode" => {
            if let Some(rest) = surface_token.strip_prefix("vscode-window:") {
                if rest.is_empty() {
                    return "VS Code window (unknown)".to_string();
                }
                return format!("VS Code window {}", rest);
            }
            surface_token.to_string()
        }
        _ => surface_token.to_string(),
    }
}

/// Human-display label for a project_anchor. For paths, basename.
/// For browser conversation slugs (no slash), truncate to first 8 chars
/// + "…". None if input is None.
pub fn project_label(project_anchor: Option<&str>) -> Option<String> {
    let anchor = project_anchor?;
    if anchor.contains('/') {
        // Treat as path. Basename, stripping trailing slashes.
        let trimmed = anchor.trim_end_matches('/');
        let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
        if base.is_empty() {
            return Some(anchor.to_string());
        }
        return Some(base.to_string());
    }
    // Browser slug or identifier — truncate if long.
    if anchor.chars().count() > 8 {
        let cut: String = anchor.chars().take(8).collect();
        Some(format!("{}\u{2026}", cut))
    } else {
        Some(anchor.to_string())
    }
}

/// Parse out (host, path) from a URL. Returns None on non-http(s) URLs
/// or when the URL is malformed enough that we can't find the host.
fn parse_host_and_path(url: &str) -> Option<(String, String)> {
    let after_scheme = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host_with_port, path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, "/"),
    };
    // Strip explicit port (e.g. "claude.ai:443") so host matches succeed.
    let host = host_with_port
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host_with_port);
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- cli_surface_token ---

    #[test]
    fn cli_token_uses_tmux_when_present() {
        let t = cli_surface_token(
            Some("tmux"),
            Some("zestful"),
            Some("%0"),
            Some("window:ttys000/tab:1"),
        );
        assert_eq!(t.as_deref(), Some("tmux:zestful/pane:%0"));
    }

    #[test]
    fn cli_token_falls_back_to_application_instance() {
        let t = cli_surface_token(None, None, None, Some("window:ttys000/tab:1"));
        assert_eq!(t.as_deref(), Some("window:ttys000/tab:1"));
    }

    #[test]
    fn cli_token_returns_none_when_nothing_available() {
        let t = cli_surface_token(None, None, None, None);
        assert_eq!(t, None);
    }

    #[test]
    fn cli_token_uses_app_instance_when_tmux_kind_but_missing_fields() {
        // If subapp.kind is tmux but session/pane missing, fall back.
        let t = cli_surface_token(
            Some("tmux"),
            None,
            None,
            Some("window:ttys000/tab:1"),
        );
        assert_eq!(t.as_deref(), Some("window:ttys000/tab:1"));
    }

    // --- browser_conversation_slug ---

    #[test]
    fn browser_slug_claude_ai() {
        assert_eq!(
            browser_conversation_slug("https://claude.ai/chats/abc-123-def").as_deref(),
            Some("abc-123-def")
        );
    }

    #[test]
    fn browser_slug_chatgpt() {
        assert_eq!(
            browser_conversation_slug("https://chatgpt.com/c/xyz789").as_deref(),
            Some("xyz789")
        );
    }

    #[test]
    fn browser_slug_chatgpt_legacy_host() {
        assert_eq!(
            browser_conversation_slug("https://chat.openai.com/c/xyz789").as_deref(),
            Some("xyz789")
        );
    }

    #[test]
    fn browser_slug_gemini() {
        assert_eq!(
            browser_conversation_slug("https://gemini.google.com/app/q1w2e3").as_deref(),
            Some("q1w2e3")
        );
    }

    #[test]
    fn browser_slug_homepage_returns_none() {
        assert_eq!(browser_conversation_slug("https://claude.ai/"), None);
        assert_eq!(browser_conversation_slug("https://chatgpt.com/"), None);
    }

    #[test]
    fn browser_slug_unknown_host_returns_none() {
        assert_eq!(browser_conversation_slug("https://example.com/c/123"), None);
    }

    #[test]
    fn browser_slug_malformed_returns_none() {
        assert_eq!(browser_conversation_slug("not-a-url"), None);
        assert_eq!(browser_conversation_slug(""), None);
    }

    #[test]
    fn browser_slug_strips_query_and_fragment() {
        assert_eq!(
            browser_conversation_slug("https://claude.ai/chats/abc?ref=email#top").as_deref(),
            Some("abc")
        );
    }

    // --- browser_agent_for_url ---

    #[test]
    fn browser_agent_for_each_known_host() {
        assert_eq!(browser_agent_for_url("https://claude.ai/").as_deref(), Some("claude-web"));
        assert_eq!(browser_agent_for_url("https://chatgpt.com/").as_deref(), Some("chatgpt-web"));
        assert_eq!(browser_agent_for_url("https://chat.openai.com/").as_deref(), Some("chatgpt-web"));
        assert_eq!(browser_agent_for_url("https://gemini.google.com/").as_deref(), Some("gemini-web"));
    }

    #[test]
    fn browser_agent_unknown_returns_none() {
        assert_eq!(browser_agent_for_url("https://example.com/"), None);
        assert_eq!(browser_agent_for_url("not-a-url"), None);
    }

    // --- vscode_surface_token ---

    #[test]
    fn vscode_token_format() {
        assert_eq!(vscode_surface_token("12345"), "vscode-window:12345");
    }

    // --- surface_label ---

    #[test]
    fn surface_label_cli_tmux() {
        assert_eq!(surface_label("cli", "tmux:zestful/pane:%0"), "tmux zestful → pane %0");
    }

    #[test]
    fn surface_label_cli_iterm() {
        assert_eq!(
            surface_label("cli", "window:ttys000/tab:1"),
            "iTerm2 window ttys000 / tab 1"
        );
    }

    #[test]
    fn surface_label_browser_truncates_long_slug() {
        let label = surface_label("browser", "abc12345extra");
        assert!(label.contains("conversation"), "label = {}", label);
        // Should truncate to ~8 chars + ellipsis.
        assert!(label.contains("abc12345"), "label = {}", label);
    }

    #[test]
    fn surface_label_vscode() {
        assert_eq!(surface_label("vscode", "vscode-window:1234"), "VS Code window 1234");
    }

    #[test]
    fn surface_label_unknown_kind_returns_token_as_is() {
        assert_eq!(surface_label("alien", "something"), "something");
    }

    // --- project_label ---

    #[test]
    fn project_label_path_returns_basename() {
        assert_eq!(project_label(Some("/Users/x/Development/Fubar")).as_deref(), Some("Fubar"));
    }

    #[test]
    fn project_label_path_with_trailing_slash() {
        assert_eq!(project_label(Some("/Users/x/Development/Fubar/")).as_deref(), Some("Fubar"));
    }

    #[test]
    fn project_label_browser_slug_truncates() {
        let label = project_label(Some("abcdef1234567890")).unwrap();
        assert!(label.starts_with("abcdef12"), "label = {}", label);
        assert!(label.ends_with("…"), "label = {}", label);
    }

    #[test]
    fn project_label_short_slug_no_truncation() {
        let label = project_label(Some("abc")).unwrap();
        assert_eq!(label, "abc");
    }

    #[test]
    fn project_label_none_returns_none() {
        assert_eq!(project_label(None), None);
    }

    // --- Edge cases caught in code review ---

    #[test]
    fn browser_slug_with_port_in_url() {
        // Port should be stripped from host so the match arm fires.
        assert_eq!(
            browser_conversation_slug("https://claude.ai:443/chats/abc-123").as_deref(),
            Some("abc-123")
        );
    }

    #[test]
    fn browser_slug_chats_no_trailing_slash_returns_none() {
        // /chats (without trailing /) is NOT a conversation URL.
        assert_eq!(browser_conversation_slug("https://claude.ai/chats"), None);
    }

    #[test]
    fn surface_label_browser_empty_token() {
        // Defensive: empty token shouldn't produce trailing space.
        assert_eq!(surface_label("browser", ""), "conversation");
    }

    #[test]
    fn surface_label_vscode_empty_pid() {
        // Defensive: empty pid in vscode-window: shouldn't produce trailing space.
        assert_eq!(surface_label("vscode", "vscode-window:"), "VS Code window (unknown)");
    }
}
