//! Per-event derivation: take an EventRow + the rolling VS Code
//! "what view is currently active in each window" map, return either
//! Some(DerivedRow) describing the contributing tile tuple, or None
//! if the event lacks enough signal to identify a tile.

use crate::events::store::query::EventRow;
use crate::events::tiles::surfaces;
use std::collections::HashMap;

/// Output of derive(). Contributes to one tile when grouped with other
/// rows sharing the same (agent, project_anchor, surface_token).
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedRow {
    pub agent: String,
    pub project_anchor: String,
    pub surface_kind: String,    // "cli" | "browser" | "vscode"
    pub surface_token: String,
    pub received_at: i64,
    pub event_type: String,
    pub focus_uri: Option<String>,
}

/// Map from VS Code window pid → currently-visible view name. Updated
/// in compute() as we walk events in received_at ASC order.
pub type VscodeAttribution = HashMap<String, String>;

pub fn derive(row: &EventRow, vscode_views: &VscodeAttribution) -> Option<DerivedRow> {
    let context = row.context.as_ref()?;
    let payload = row.payload.as_ref();

    let focus_uri = context.get("focus_uri").and_then(|v| v.as_str()).map(String::from);

    // --- Browser path ---
    if row.source == "chrome-extension" {
        let url = payload?.get("url").and_then(|v| v.as_str())?;
        let agent = surfaces::browser_agent_for_url(url)?;
        let slug = surfaces::browser_conversation_slug(url)?;
        return Some(DerivedRow {
            agent,
            project_anchor: slug.clone(),
            surface_kind: "browser".to_string(),
            surface_token: slug,
            received_at: row.received_at,
            event_type: row.event_type.clone(),
            focus_uri,
        });
    }

    // --- VS Code path ---
    if row.source == "vscode-extension" {
        let window_pid = context.get("application_instance").and_then(|v| v.as_str())?;
        let agent = match row.event_type.as_str() {
            "editor.view.visible" => {
                let view = payload?.get("view").and_then(|v| v.as_str())?;
                // Missing `visible` is unrecoverable — match parse_view_visible_change.
                let visible = payload?.get("visible").and_then(|v| v.as_bool())?;
                if !visible { return None; }
                format!("vscode+{}", view)
            }
            "editor.window.focused" => {
                let view = vscode_views.get(window_pid)?;
                format!("vscode+{}", view)
            }
            _ => return None,
        };
        let project_anchor = context
            .get("workspace_root")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())?
            .to_string();
        let surface_token = surfaces::vscode_surface_token(window_pid);
        return Some(DerivedRow {
            agent,
            project_anchor,
            surface_kind: "vscode".to_string(),
            surface_token,
            received_at: row.received_at,
            event_type: row.event_type.clone(),
            focus_uri,
        });
    }

    // --- CLI / terminal path (default for any source not handled above) ---
    // Any event whose source isn't "chrome-extension" or "vscode-extension"
    // falls through here; new emitters that don't fit are silently classified
    // as CLI. If a new source needs different handling, add a dispatch arm above.
    let agent = context.get("agent").and_then(|v| v.as_str())?.to_string();

    // Project anchor priority: env vars → workspace_root → cwd.
    let project_anchor = {
        let env = context.get("env_vars_observed");
        let from_claude = env
            .and_then(|e| e.get("CLAUDE_PROJECT_DIR"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|s: &String| !s.is_empty());
        let from_gemini = env
            .and_then(|e| e.get("GEMINI_PROJECT_DIR"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|s: &String| !s.is_empty());
        let from_workspace = context
            .get("workspace_root")
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|s: &String| !s.is_empty());
        let from_cwd = context
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|s: &String| !s.is_empty());
        from_claude
            .or(from_gemini)
            .or(from_workspace)
            .or(from_cwd)?
    };

    // Surface token: tmux preferred, app_instance fallback.
    let subapp = context.get("subapplication");
    let subapp_kind = subapp.and_then(|s| s.get("kind")).and_then(|v| v.as_str());
    let subapp_session = subapp.and_then(|s| s.get("session")).and_then(|v| v.as_str());
    let subapp_pane = subapp.and_then(|s| s.get("pane")).and_then(|v| v.as_str());
    let app_instance = context.get("application_instance").and_then(|v| v.as_str());
    let surface_token = surfaces::cli_surface_token(subapp_kind, subapp_session, subapp_pane, app_instance)?;

    Some(DerivedRow {
        agent,
        project_anchor,
        surface_kind: "cli".to_string(),
        surface_token,
        received_at: row.received_at,
        event_type: row.event_type.clone(),
        focus_uri,
    })
}

/// Helper: peek at editor.view.visible events to update the rolling
/// attribution map. Called by compute() before derive(). Returns
/// Some((window_pid, view, visible)) for view.visible events; None
/// otherwise. compute() decides to insert/remove based on visible flag.
pub fn parse_view_visible_change(row: &EventRow) -> Option<(String, String, bool)> {
    if row.source != "vscode-extension" || row.event_type != "editor.view.visible" {
        return None;
    }
    let context = row.context.as_ref()?;
    let payload = row.payload.as_ref()?;
    let window_pid = context.get("application_instance").and_then(|v| v.as_str())?.to_string();
    let view = payload.get("view").and_then(|v| v.as_str())?.to_string();
    // Missing `visible` is unrecoverable — return None rather than
    // defaulting to false, which would be interpreted by compute()
    // as an explicit hide and mutate the rolling map.
    let visible = payload.get("visible").and_then(|v| v.as_bool())?;
    Some((window_pid, view, visible))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eventrow(id: i64, source: &str, event_type: &str, context: serde_json::Value, payload: serde_json::Value, received_at: i64) -> EventRow {
        EventRow {
            id,
            received_at,
            event_id: format!("evt-{}", id),
            event_type: event_type.to_string(),
            source: source.to_string(),
            session_id: context.get("session_id").and_then(|v| v.as_str()).map(String::from),
            project: context.get("project").and_then(|v| v.as_str()).map(String::from),
            host: "h".to_string(),
            os_user: "u".to_string(),
            device_id: "d".to_string(),
            event_ts: received_at,
            seq: 0,
            source_pid: 1,
            schema_version: 1,
            correlation: None,
            context: Some(context),
            payload: Some(payload),
        }
    }

    // --- agent + project priority chains for CLI ---

    #[test]
    fn derive_claude_code_event_anchored_on_env_var() {
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/x/sub/deeper",
            "workspace_root": "/x/sub",
            "env_vars_observed": { "CLAUDE_PROJECT_DIR": "/x" },
            "subapplication": { "kind": "tmux", "session": "zestful", "pane": "%0" }
        });
        let r = eventrow(1, "claude-code", "turn.completed", ctx, json!({}), 1000);
        let d = derive(&r, &VscodeAttribution::new()).expect("expected Some");
        assert_eq!(d.agent, "claude-code");
        assert_eq!(d.project_anchor, "/x");
        assert_eq!(d.surface_kind, "cli");
        assert_eq!(d.surface_token, "tmux:zestful/pane:%0");
    }

    #[test]
    fn derive_falls_back_to_workspace_root_when_no_env_var() {
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/x/sub",
            "workspace_root": "/x",
            "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
        });
        let r = eventrow(2, "claude-code", "turn.completed", ctx, json!({}), 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.project_anchor, "/x");
    }

    #[test]
    fn derive_falls_back_to_cwd_when_no_env_var_or_workspace() {
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/x/sub",
            "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
        });
        let r = eventrow(3, "claude-code", "turn.completed", ctx, json!({}), 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.project_anchor, "/x/sub");
    }

    #[test]
    fn derive_skips_event_with_no_project_signal() {
        let ctx = json!({
            "agent": "claude-code",
            "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
        });
        let r = eventrow(4, "claude-code", "turn.completed", ctx, json!({}), 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    #[test]
    fn derive_skips_event_with_no_surface_signal() {
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/x"
        });
        let r = eventrow(5, "claude-code", "turn.completed", ctx, json!({}), 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    // --- gemini env var ---

    #[test]
    fn derive_uses_gemini_project_dir() {
        let ctx = json!({
            "agent": "gemini-cli",
            "cwd": "/x/sub",
            "env_vars_observed": { "GEMINI_PROJECT_DIR": "/x" },
            "application_instance": "window:ttys000/tab:1"
        });
        let r = eventrow(6, "gemini-cli", "turn.completed", ctx, json!({}), 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.project_anchor, "/x");
    }

    // --- browser ---

    #[test]
    fn derive_browser_event_extracts_conversation_slug_and_agent() {
        let ctx = json!({});
        let payload = json!({ "url": "https://claude.ai/chats/abc-123" });
        let r = eventrow(7, "chrome-extension", "agent.notified", ctx, payload, 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.agent, "claude-web");
        assert_eq!(d.project_anchor, "abc-123");
        assert_eq!(d.surface_kind, "browser");
        assert_eq!(d.surface_token, "abc-123");
    }

    #[test]
    fn derive_browser_event_with_no_conversation_url_returns_none() {
        let ctx = json!({});
        let payload = json!({ "url": "https://claude.ai/" });
        let r = eventrow(8, "chrome-extension", "agent.notified", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    #[test]
    fn derive_browser_event_with_unknown_host_returns_none() {
        let ctx = json!({});
        let payload = json!({ "url": "https://example.com/" });
        let r = eventrow(9, "chrome-extension", "agent.notified", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    // --- vscode ---

    #[test]
    fn derive_vscode_view_visible_attributes_agent() {
        let ctx = json!({
            "application_instance": "12345",
            "workspace_root": "/x/Wibble"
        });
        let payload = json!({ "view": "openai.chatgpt", "visible": true });
        let r = eventrow(10, "vscode-extension", "editor.view.visible", ctx, payload, 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.agent, "vscode+openai.chatgpt");
        assert_eq!(d.project_anchor, "/x/Wibble");
        assert_eq!(d.surface_kind, "vscode");
        assert_eq!(d.surface_token, "vscode-window:12345");
    }

    #[test]
    fn derive_vscode_view_hide_returns_none_for_tile_purposes() {
        // A view-hidden event tells us state changed, but it doesn't
        // identify an active tile by itself.
        let ctx = json!({
            "application_instance": "12345",
            "workspace_root": "/x/Wibble"
        });
        let payload = json!({ "view": "openai.chatgpt", "visible": false });
        let r = eventrow(11, "vscode-extension", "editor.view.visible", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    #[test]
    fn derive_vscode_window_focused_attributes_via_rolling_map() {
        let ctx = json!({
            "application_instance": "12345",
            "workspace_root": "/x/Wibble"
        });
        let payload = json!({});
        let r = eventrow(12, "vscode-extension", "editor.window.focused", ctx, payload, 1000);
        let mut views = VscodeAttribution::new();
        views.insert("12345".to_string(), "openai.chatgpt".to_string());
        let d = derive(&r, &views).unwrap();
        assert_eq!(d.agent, "vscode+openai.chatgpt");
    }

    #[test]
    fn derive_vscode_window_focused_with_no_attribution_returns_none() {
        let ctx = json!({
            "application_instance": "12345",
            "workspace_root": "/x/Wibble"
        });
        let payload = json!({});
        let r = eventrow(13, "vscode-extension", "editor.window.focused", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    // --- focus_uri propagation ---

    #[test]
    fn derive_carries_focus_uri_when_present() {
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/x",
            "focus_uri": "workspace://iterm2/window:1/tab:2",
            "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
        });
        let r = eventrow(14, "claude-code", "turn.completed", ctx, json!({}), 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.focus_uri.as_deref(), Some("workspace://iterm2/window:1/tab:2"));
    }

    // --- parse_view_visible_change ---

    #[test]
    fn parse_view_visible_change_visible_true() {
        let ctx = json!({ "application_instance": "12345" });
        let payload = json!({ "view": "openai.chatgpt", "visible": true });
        let r = eventrow(15, "vscode-extension", "editor.view.visible", ctx, payload, 1000);
        let parsed = parse_view_visible_change(&r).unwrap();
        assert_eq!(parsed.0, "12345");
        assert_eq!(parsed.1, "openai.chatgpt");
        assert!(parsed.2);
    }

    #[test]
    fn parse_view_visible_change_visible_false() {
        let ctx = json!({ "application_instance": "12345" });
        let payload = json!({ "view": "openai.chatgpt", "visible": false });
        let r = eventrow(16, "vscode-extension", "editor.view.visible", ctx, payload, 1000);
        let parsed = parse_view_visible_change(&r).unwrap();
        assert!(!parsed.2);
    }

    #[test]
    fn parse_view_visible_change_for_unrelated_event_returns_none() {
        let ctx = json!({ "application_instance": "12345" });
        let payload = json!({});
        let r = eventrow(17, "vscode-extension", "editor.window.focused", ctx, payload, 1000);
        assert!(parse_view_visible_change(&r).is_none());
    }

    // --- Edge cases caught in code review ---

    #[test]
    fn derive_returns_none_when_context_is_none() {
        let mut r = eventrow(20, "claude-code", "turn.completed", json!({}), json!({}), 1000);
        r.context = None;
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    #[test]
    fn derive_chrome_extension_with_none_payload_returns_none() {
        let mut r = eventrow(21, "chrome-extension", "agent.notified", json!({}), json!({}), 1000);
        r.payload = None;
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    #[test]
    fn derive_vscode_with_unknown_event_type_returns_none() {
        let ctx = json!({
            "application_instance": "12345",
            "workspace_root": "/x/Wibble"
        });
        let payload = json!({});
        let r = eventrow(22, "vscode-extension", "editor.something_unknown", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new()).is_none());
    }

    #[test]
    fn derive_cli_skips_empty_string_env_var() {
        // Empty-string env var should NOT win the priority chain;
        // workspace_root or cwd should be used instead.
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/real/path",
            "env_vars_observed": { "CLAUDE_PROJECT_DIR": "" },
            "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
        });
        let r = eventrow(23, "claude-code", "turn.completed", ctx, json!({}), 1000);
        let d = derive(&r, &VscodeAttribution::new()).unwrap();
        assert_eq!(d.project_anchor, "/real/path");
    }

    #[test]
    fn parse_view_visible_change_for_non_vscode_source_returns_none() {
        let ctx = json!({ "application_instance": "12345" });
        let payload = json!({ "view": "openai.chatgpt", "visible": true });
        let r = eventrow(24, "claude-code", "editor.view.visible", ctx, payload, 1000);
        assert!(parse_view_visible_change(&r).is_none());
    }

    #[test]
    fn parse_view_visible_change_with_missing_visible_returns_none() {
        // Missing `visible` is unrecoverable — return None rather than
        // defaulting to false (which would mutate compute()'s rolling map).
        let ctx = json!({ "application_instance": "12345" });
        let payload = json!({ "view": "openai.chatgpt" });
        let r = eventrow(25, "vscode-extension", "editor.view.visible", ctx, payload, 1000);
        assert!(parse_view_visible_change(&r).is_none());
    }
}
