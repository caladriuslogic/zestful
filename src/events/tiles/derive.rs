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

/// How recent (in unix milliseconds) a vscode-extension focus signal must
/// be relative to a Codex event for the projection to attribute that
/// Codex event to the focused VS Code window. 5 seconds covers the
/// "user is actively typing in VS Code right now" case while excluding
/// stale focus signals from earlier in the session.
pub const CORRELATION_WINDOW_MS: i64 = 5_000;

/// Sentinel project_anchor for standalone Codex.app tiles. Used so the
/// existing (agent, project_anchor, surface_token) tile identity tuple
/// collapses all standalone-Codex events into a single tile regardless
/// of which task folder Codex.app is currently working in.
pub const STANDALONE_CODEX_ANCHOR: &str = "<codex-app>";

/// Sentinel surface_token for standalone Codex.app tiles. Pairs with
/// STANDALONE_CODEX_ANCHOR.
pub const STANDALONE_CODEX_SURFACE: &str = "codex";

/// Rolling state: the most recent vscode-extension focus signal observed
/// during a `walk_and_derive` pass. Updated on each `editor.window.focused`
/// or `editor.view.visible visible=true` event from `vscode-extension`.
/// Used by `derive()` to attribute Codex events to a VS Code window when
/// they occur within a short time-correlation window of a focus signal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VscodeRecentFocus {
    /// `received_at` of the most recent qualifying focus event, unix ms.
    pub ts_ms: Option<i64>,
    /// `context.application_instance` from that event — the VS Code window pid.
    pub window_pid: Option<String>,
    /// `context.workspace_root` from that event.
    pub workspace_root: Option<String>,
}

pub fn derive(
    row: &EventRow,
    vscode_views: &VscodeAttribution,
    vscode_focus: &VscodeRecentFocus,
) -> Option<DerivedRow> {
    // Codex events: attribute via temporal correlation with the most recent
    // vscode-extension focus signal. Within CORRELATION_WINDOW_MS → VS-Code
    // attributed tile; otherwise standalone Codex.app tile (collapsed across
    // all tasks).
    if row.source == "codex" {
        // Synthesize a focus_uri that matches the new attribution. We
        // intentionally do NOT preserve `row.context.focus_uri`: that field
        // was set at ingest time (possibly by the legacy hook routing) and
        // can carry a stale interpretation that contradicts the projection's
        // current attribution.
        let correlated = vscode_focus
            .ts_ms
            .map(|ts| row.received_at >= ts && row.received_at - ts <= CORRELATION_WINDOW_MS)
            .unwrap_or(false);
        if correlated {
            let window_pid = vscode_focus.window_pid.clone().unwrap_or_default();
            let workspace_root = vscode_focus.workspace_root.clone().unwrap_or_default();
            // workspace://vscode/window:<pid>/project:<basename> for click-to-focus.
            let project_basename = std::path::Path::new(&workspace_root)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let focus_uri = if !project_basename.is_empty() {
                Some(format!("workspace://vscode/window:{}/project:{}", window_pid, project_basename))
            } else {
                Some(format!("workspace://vscode/window:{}", window_pid))
            };
            return Some(DerivedRow {
                agent: "codex".to_string(),
                project_anchor: workspace_root,
                surface_kind: "cli".to_string(),
                surface_token: format!("window:{}", window_pid),
                received_at: row.received_at,
                event_type: row.event_type.clone(),
                focus_uri,
            });
        }
        return Some(DerivedRow {
            agent: "codex".to_string(),
            project_anchor: STANDALONE_CODEX_ANCHOR.to_string(),
            surface_kind: "cli".to_string(),
            surface_token: STANDALONE_CODEX_SURFACE.to_string(),
            received_at: row.received_at,
            event_type: row.event_type.clone(),
            // Standalone Codex.app — Mac app activation URI.
            focus_uri: Some("workspace://codex".to_string()),
        });
    }

    let context = row.context.as_ref()?;
    let payload = row.payload.as_ref();

    let focus_uri = context.get("focus_uri").and_then(|v| v.as_str()).map(String::from);

    // --- Browser path ---
    if row.source == "chrome-extension" {
        // Chrome extension emits the conversation URL in
        // `context.focus_uri`, not `payload.url`. The projection reuses
        // the focus_uri already extracted from context above.
        let url = focus_uri.as_deref()?;
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

/// Helper: extract the focus signal `(window_pid, workspace_root, received_at)`
/// from a vscode-extension event that indicates the user is currently driving
/// a particular VS Code window. Returns `Some` for:
///   - `editor.window.focused` events
///   - `editor.view.visible` events with `payload.visible == true`
///
/// Returns `None` for any other event source/type, or when the event is
/// missing `context.application_instance` or `context.workspace_root`.
pub fn parse_vscode_focus_signal(row: &EventRow) -> Option<(String, String, i64)> {
    if row.source != "vscode-extension" {
        return None;
    }
    match row.event_type.as_str() {
        "editor.window.focused" => {}
        "editor.view.visible" => {
            // Only `visible: true` qualifies as a focus signal.
            let payload = row.payload.as_ref()?;
            let visible = payload.get("visible").and_then(|v| v.as_bool())?;
            if !visible {
                return None;
            }
        }
        _ => return None,
    }
    let context = row.context.as_ref()?;
    let window_pid = context
        .get("application_instance")
        .and_then(|v| v.as_str())?
        .to_string();
    let workspace_root = context
        .get("workspace_root")
        .and_then(|v| v.as_str())?
        .to_string();
    Some((window_pid, workspace_root, row.received_at))
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).expect("expected Some");
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
        assert_eq!(d.project_anchor, "/x/sub");
    }

    #[test]
    fn derive_skips_event_with_no_project_signal() {
        let ctx = json!({
            "agent": "claude-code",
            "subapplication": { "kind": "tmux", "session": "z", "pane": "%0" }
        });
        let r = eventrow(4, "claude-code", "turn.completed", ctx, json!({}), 1000);
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
    }

    #[test]
    fn derive_skips_event_with_no_surface_signal() {
        let ctx = json!({
            "agent": "claude-code",
            "cwd": "/x"
        });
        let r = eventrow(5, "claude-code", "turn.completed", ctx, json!({}), 1000);
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
        assert_eq!(d.project_anchor, "/x");
    }

    // --- browser ---

    #[test]
    fn derive_browser_event_extracts_conversation_slug_and_agent() {
        // Production shape: chrome ext puts the URL in context.focus_uri.
        let ctx = json!({ "focus_uri": "https://claude.ai/chats/abc-123" });
        let payload = json!({ "kind": "notification" });
        let r = eventrow(7, "chrome-extension", "agent.notified", ctx, payload, 1000);
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
        assert_eq!(d.agent, "claude-web");
        assert_eq!(d.project_anchor, "abc-123");
        assert_eq!(d.surface_kind, "browser");
        assert_eq!(d.surface_token, "abc-123");
    }

    /// Regression: chrome extension emits `context.focus_uri` containing
    /// the chat URL, NOT `payload.url`. Projection must read from there.
    /// Prior bug: derive() looked for `payload.url` only and dropped the
    /// row, so no browser tile ever appeared while the user was actively
    /// chatting on chatgpt.com.
    #[test]
    fn derive_browser_event_reads_url_from_context_focus_uri() {
        // This is the actual shape the chrome extension produces today
        // (verified against events.db on 2026-04-26):
        let ctx = json!({
            "agent": "chatgpt",
            "focus_uri": "https://chatgpt.com/c/69ebcb2a-675c-83e8-9920-55b333f1aa2b",
        });
        let payload = json!({
            "kind": "notification",
            "message": "Response complete — I'm not sure about that",
        });
        let r = eventrow(99, "chrome-extension", "agent.notified", ctx, payload, 1000);
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default())
            .expect("expected DerivedRow — projection must accept the chrome ext's actual event shape");
        assert_eq!(d.agent, "chatgpt-web");
        assert_eq!(d.project_anchor, "69ebcb2a-675c-83e8-9920-55b333f1aa2b");
        assert_eq!(d.surface_kind, "browser");
        assert_eq!(d.surface_token, "69ebcb2a-675c-83e8-9920-55b333f1aa2b");
    }

    #[test]
    fn derive_browser_event_with_no_conversation_url_returns_none() {
        // URL exists but isn't a conversation URL (no /chats/<slug> path).
        let ctx = json!({ "focus_uri": "https://claude.ai/" });
        let payload = json!({ "kind": "notification" });
        let r = eventrow(8, "chrome-extension", "agent.notified", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
    }

    #[test]
    fn derive_browser_event_with_unknown_host_returns_none() {
        let ctx = json!({ "focus_uri": "https://example.com/" });
        let payload = json!({ "kind": "notification" });
        let r = eventrow(9, "chrome-extension", "agent.notified", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
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
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
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
        let d = derive(&r, &views, &VscodeRecentFocus::default()).unwrap();
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
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
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
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
    }

    #[test]
    fn derive_chrome_extension_with_none_payload_returns_none() {
        let mut r = eventrow(21, "chrome-extension", "agent.notified", json!({}), json!({}), 1000);
        r.payload = None;
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
    }

    #[test]
    fn derive_vscode_with_unknown_event_type_returns_none() {
        let ctx = json!({
            "application_instance": "12345",
            "workspace_root": "/x/Wibble"
        });
        let payload = json!({});
        let r = eventrow(22, "vscode-extension", "editor.something_unknown", ctx, payload, 1000);
        assert!(derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).is_none());
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
        let d = derive(&r, &VscodeAttribution::new(), &VscodeRecentFocus::default()).unwrap();
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

    #[test]
    fn parse_vscode_focus_signal_returns_signal_for_window_focused() {
        let r = eventrow(
            1,
            "vscode-extension",
            "editor.window.focused",
            json!({ "application_instance": "80836", "workspace_root": "/x/zestful" }),
            json!({}),
            5_000,
        );
        let result = parse_vscode_focus_signal(&r);
        assert_eq!(
            result,
            Some(("80836".to_string(), "/x/zestful".to_string(), 5_000))
        );
    }

    #[test]
    fn parse_vscode_focus_signal_returns_signal_for_view_visible_true() {
        let r = eventrow(
            2,
            "vscode-extension",
            "editor.view.visible",
            json!({ "application_instance": "80900", "workspace_root": "/x/other" }),
            json!({ "view": "openai.codex", "visible": true }),
            6_000,
        );
        let result = parse_vscode_focus_signal(&r);
        assert_eq!(
            result,
            Some(("80900".to_string(), "/x/other".to_string(), 6_000))
        );
    }

    #[test]
    fn parse_vscode_focus_signal_returns_none_for_view_visible_false() {
        let r = eventrow(
            3,
            "vscode-extension",
            "editor.view.visible",
            json!({ "application_instance": "80900", "workspace_root": "/x/other" }),
            json!({ "view": "openai.codex", "visible": false }),
            7_000,
        );
        assert_eq!(parse_vscode_focus_signal(&r), None);
    }

    #[test]
    fn parse_vscode_focus_signal_returns_none_for_non_vscode_source() {
        let r = eventrow(
            4,
            "claude-code",
            "editor.window.focused",
            json!({ "application_instance": "80900", "workspace_root": "/x/other" }),
            json!({}),
            8_000,
        );
        assert_eq!(parse_vscode_focus_signal(&r), None);
    }

    #[test]
    fn parse_vscode_focus_signal_returns_none_when_application_instance_missing() {
        let r = eventrow(
            5,
            "vscode-extension",
            "editor.window.focused",
            json!({ "workspace_root": "/x/other" }),  // no application_instance
            json!({}),
            9_000,
        );
        assert_eq!(parse_vscode_focus_signal(&r), None);
    }

    fn codex_event(id: i64, ts: i64, cwd: &str) -> EventRow {
        eventrow(
            id,
            "codex",
            "turn.completed",
            json!({ "agent": "codex", "cwd": cwd, "workspace_root": cwd, "subapplication": null }),
            json!({}),
            ts,
        )
    }

    #[test]
    fn derive_codex_correlates_with_recent_vscode_focus_within_5s() {
        let focus = VscodeRecentFocus {
            ts_ms: Some(1_000),
            window_pid: Some("80836".to_string()),
            workspace_root: Some("/x/zestful".to_string()),
        };
        let codex = codex_event(1, 2_000, "/Users/x/Documents/Codex/abc");
        let d = derive(&codex, &VscodeAttribution::new(), &focus)
            .expect("expected DerivedRow");
        assert_eq!(d.agent, "codex");
        assert_eq!(d.project_anchor, "/x/zestful");
        assert_eq!(d.surface_kind, "cli");
        assert_eq!(d.surface_token, "window:80836");
        // Synthesized focus_uri: workspace_root basename = "zestful".
        assert_eq!(
            d.focus_uri.as_deref(),
            Some("workspace://vscode/window:80836/project:zestful")
        );
    }

    #[test]
    fn derive_codex_falls_back_to_standalone_when_focus_too_old() {
        let focus = VscodeRecentFocus {
            ts_ms: Some(0),
            window_pid: Some("80836".to_string()),
            workspace_root: Some("/x/zestful".to_string()),
        };
        // 10 seconds later — outside 5s correlation window.
        let codex = codex_event(1, 10_000, "/Users/x/Documents/Codex/abc");
        let d = derive(&codex, &VscodeAttribution::new(), &focus)
            .expect("expected DerivedRow");
        assert_eq!(d.agent, "codex");
        assert_eq!(d.project_anchor, STANDALONE_CODEX_ANCHOR);
        assert_eq!(d.surface_kind, "cli");
        assert_eq!(d.surface_token, STANDALONE_CODEX_SURFACE);
        assert_eq!(d.focus_uri.as_deref(), Some("workspace://codex"));
    }

    #[test]
    fn derive_codex_falls_back_to_standalone_when_no_focus() {
        let focus = VscodeRecentFocus::default();
        let codex = codex_event(1, 5_000, "/Users/x/Documents/Codex/abc");
        let d = derive(&codex, &VscodeAttribution::new(), &focus)
            .expect("expected DerivedRow");
        assert_eq!(d.project_anchor, STANDALONE_CODEX_ANCHOR);
        assert_eq!(d.surface_token, STANDALONE_CODEX_SURFACE);
        assert_eq!(d.focus_uri.as_deref(), Some("workspace://codex"));
    }

    #[test]
    fn derive_codex_does_not_carry_stale_event_focus_uri() {
        // Even when the event's context carries a focus_uri (e.g. from a legacy
        // hook ingestion), the standalone branch synthesizes its own.
        let focus = VscodeRecentFocus::default();
        let codex = eventrow(
            1,
            "codex",
            "turn.completed",
            json!({
                "agent": "codex",
                "cwd": "/Users/x/Documents/Codex/abc",
                "focus_uri": "workspace://vscode/window:99/project:wrong",
            }),
            json!({}),
            5_000,
        );
        let d = derive(&codex, &VscodeAttribution::new(), &focus)
            .expect("expected DerivedRow");
        assert_eq!(d.focus_uri.as_deref(), Some("workspace://codex"));
    }
}
