//! Tiles projection — derives a minimal set of "agent instance" tiles
//! from the last N hours of events on demand. See spec
//! 2026-04-23-tiles-projection-design.md.

pub mod cluster;
pub mod derive;
pub mod surfaces;
pub mod tile;

use crate::events::store::query::EventRow;
use rusqlite::Connection;

/// Compute the tile projection over events with received_at >= since_ms.
/// Pure function over the events table — no caching, no incremental
/// state. Each call re-scans and re-derives.
///
/// Returns tiles sorted by last_seen_at descending.
pub fn compute(conn: &Connection, since_ms: i64) -> rusqlite::Result<Vec<tile::Tile>> {
    let rows = fetch_since(conn, since_ms)?;
    let derived = walk_and_derive(&rows);
    let tiles = cluster::group(&derived);
    Ok(tiles)
}

/// Fetch all events with received_at >= since_ms, ordered ASC by
/// received_at, then by id ASC for stable tiebreaking.
pub(crate) fn fetch_since(conn: &Connection, since_ms: i64) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, received_at, event_id, event_type, source, session_id, project,
                host, os_user, device_id, event_ts, seq, source_pid, schema_version,
                correlation, context, payload
         FROM events
         WHERE received_at >= ?
         ORDER BY received_at ASC, id ASC",
    )?;
    let rows_iter = stmt.query_map([since_ms], |row| {
        Ok(EventRow {
            id: row.get(0)?,
            received_at: row.get(1)?,
            event_id: row.get(2)?,
            event_type: row.get(3)?,
            source: row.get(4)?,
            session_id: row.get(5)?,
            project: row.get(6)?,
            host: row.get(7)?,
            os_user: row.get(8)?,
            device_id: row.get(9)?,
            event_ts: row.get(10)?,
            seq: row.get(11)?,
            source_pid: row.get(12)?,
            schema_version: row.get(13)?,
            // Silently tolerate malformed JSON in the JSON columns:
            // a corrupt event drops out of the tile projection
            // (derive() returns None when context is None) rather
            // than aborting the whole projection. This diverges
            // from query.rs::parse_json_column which raises
            // FromSqlConversionFailure for the GET /events path,
            // where loud failure is more useful for debugging
            // event-pipeline issues. Tiles is best-effort.
            correlation: row.get::<_, Option<String>>(14)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            context: row.get::<_, Option<String>>(15)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            payload: row.get::<_, Option<String>>(16)?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;
    let mut out = Vec::new();
    for r in rows_iter {
        out.push(r?);
    }
    Ok(out)
}

/// Single-pass walk: maintain a rolling VS Code "currently visible
/// view per window" map; for each row, update the map if it's a
/// view.visible event, then call derive() with the current map state.
/// Returns all DerivedRows that successfully derived.
pub(crate) fn walk_and_derive(rows: &[EventRow]) -> Vec<derive::DerivedRow> {
    let mut active_views = derive::VscodeAttribution::new();
    let mut recent_focus = derive::VscodeRecentFocus::default();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some((window_pid, view, visible)) = derive::parse_view_visible_change(row) {
            if visible {
                active_views.insert(window_pid, view);
            } else {
                active_views.remove(&window_pid);
            }
        }
        if let Some((window_pid, workspace_root, ts_ms)) = derive::parse_vscode_focus_signal(row) {
            recent_focus = derive::VscodeRecentFocus {
                ts_ms: Some(ts_ms),
                window_pid: Some(window_pid),
                workspace_root: Some(workspace_root),
            };
        }
        if let Some(d) = derive::derive(row, &active_views, &recent_focus) {
            out.push(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::query::EventRow;
    use serde_json::json;

    fn er(id: i64, source: &str, event_type: &str, context: serde_json::Value, payload: serde_json::Value, ts: i64) -> EventRow {
        EventRow {
            id,
            received_at: ts,
            event_id: format!("evt-{}", id),
            event_type: event_type.to_string(),
            source: source.to_string(),
            session_id: None,
            project: None,
            host: "h".to_string(),
            os_user: "u".to_string(),
            device_id: "d".to_string(),
            event_ts: ts,
            seq: 0,
            source_pid: 1,
            schema_version: 1,
            correlation: None,
            context: Some(context),
            payload: Some(payload),
        }
    }

    fn vscode_view_visible(id: i64, window_pid: &str, view: &str, visible: bool, workspace: &str, ts: i64) -> EventRow {
        er(
            id,
            "vscode-extension",
            "editor.view.visible",
            json!({ "application_instance": window_pid, "workspace_root": workspace }),
            json!({ "view": view, "visible": visible }),
            ts,
        )
    }

    fn vscode_window_focused(id: i64, window_pid: &str, workspace: &str, ts: i64) -> EventRow {
        er(
            id,
            "vscode-extension",
            "editor.window.focused",
            json!({ "application_instance": window_pid, "workspace_root": workspace }),
            json!({}),
            ts,
        )
    }

    #[test]
    fn walk_and_derive_view_visible_then_window_focused_attributes_correctly() {
        // T=1000: view.visible visible=true for pid=W1, view=A
        // T=2000: window.focused for pid=W1
        // Expected: both events derive; the second uses the rolling map
        // to attribute its agent as "vscode+A".
        let rows = vec![
            vscode_view_visible(1, "W1", "openai.chatgpt", true, "/x", 1000),
            vscode_window_focused(2, "W1", "/x", 2000),
        ];
        let derived = walk_and_derive(&rows);
        assert_eq!(derived.len(), 2);
        assert_eq!(derived[0].agent, "vscode+openai.chatgpt");
        assert_eq!(derived[1].agent, "vscode+openai.chatgpt");
    }

    #[test]
    fn walk_and_derive_view_hidden_removes_from_map_so_focus_drops() {
        // T=1000: view.visible visible=true for W1
        // T=2000: view.visible visible=false for W1 (hides it)
        // T=3000: window.focused for W1 — should yield None
        //         (no longer in map after the hide)
        let rows = vec![
            vscode_view_visible(1, "W1", "openai.chatgpt", true, "/x", 1000),
            vscode_view_visible(2, "W1", "openai.chatgpt", false, "/x", 2000),
            vscode_window_focused(3, "W1", "/x", 3000),
        ];
        let derived = walk_and_derive(&rows);
        // Visible=true: derives.
        // Visible=false: derive() returns None for that event.
        // window.focused: derive() returns None because map empty.
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].agent, "vscode+openai.chatgpt");
        assert_eq!(derived[0].received_at, 1000);
    }

    fn codex_event(id: i64, ts: i64) -> EventRow {
        er(
            id,
            "codex",
            "turn.completed",
            json!({ "agent": "codex", "cwd": "/Users/x/Documents/Codex/abc" }),
            json!({}),
            ts,
        )
    }

    #[test]
    fn walk_and_derive_codex_with_concurrent_vscode_focus_attributes_to_vscode() {
        // vscode_window_focused has no entry in active_views map so it doesn't
        // derive a tile itself, but it does update recent_focus. The codex event
        // that follows within 5s is attributed to that VS Code window.
        let rows = vec![
            vscode_window_focused(1, "80836", "/x/zestful", 1_000),
            codex_event(2, 2_000),
        ];
        let derived = walk_and_derive(&rows);
        // Only the codex event derives (the window.focused event has no view in
        // the active_views map so derive() returns None for it).
        assert_eq!(derived.len(), 1);
        let codex_row = derived.iter().find(|d| d.agent == "codex")
            .expect("codex row");
        assert_eq!(codex_row.project_anchor, "/x/zestful");
        assert_eq!(codex_row.surface_token, "window:80836");
    }

    #[test]
    fn walk_and_derive_codex_alone_attributes_to_standalone() {
        let rows = vec![codex_event(1, 1_000)];
        let derived = walk_and_derive(&rows);
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].agent, "codex");
        assert_eq!(derived[0].project_anchor, "<codex-app>");
        assert_eq!(derived[0].surface_token, "codex");
    }

    #[test]
    fn walk_and_derive_two_codex_events_one_correlated_one_not() {
        let rows = vec![
            vscode_window_focused(1, "80836", "/x/zestful", 0),
            codex_event(2, 2_000),     // correlated (within 5s of focus)
            codex_event(3, 32_000),    // uncorrelated (focus is 32s old)
        ];
        let derived = walk_and_derive(&rows);
        let codex_rows: Vec<_> = derived.iter().filter(|d| d.agent == "codex").collect();
        assert_eq!(codex_rows.len(), 2);
        assert_eq!(codex_rows[0].project_anchor, "/x/zestful");        // correlated
        assert_eq!(codex_rows[1].project_anchor, "<codex-app>");       // standalone
    }

    #[test]
    fn walk_and_derive_two_windows_have_independent_map_state() {
        // W1 has view A visible; W2 has view B visible.
        // window.focused on W1 → vscode+A
        // window.focused on W2 → vscode+B
        // Removing W1's view doesn't affect W2.
        let rows = vec![
            vscode_view_visible(1, "W1", "view-A", true, "/x", 1000),
            vscode_view_visible(2, "W2", "view-B", true, "/y", 2000),
            vscode_window_focused(3, "W1", "/x", 3000),
            vscode_window_focused(4, "W2", "/y", 4000),
            vscode_view_visible(5, "W1", "view-A", false, "/x", 5000),
            vscode_window_focused(6, "W1", "/x", 6000),  // map empty for W1 now
            vscode_window_focused(7, "W2", "/y", 7000),  // W2 unaffected
        ];
        let derived = walk_and_derive(&rows);
        // Derives: 1 (visible=true), 2 (visible=true), 3, 4, 7 = 5 total.
        // Skipped: 5 (visible=false), 6 (W1 map empty).
        assert_eq!(derived.len(), 5, "got {:?}", derived.iter().map(|d| (d.received_at, d.agent.clone())).collect::<Vec<_>>());

        // Check pairings.
        let agent_at = |ts: i64| derived.iter().find(|d| d.received_at == ts).map(|d| d.agent.as_str());
        assert_eq!(agent_at(1000), Some("vscode+view-A"));
        assert_eq!(agent_at(2000), Some("vscode+view-B"));
        assert_eq!(agent_at(3000), Some("vscode+view-A"));
        assert_eq!(agent_at(4000), Some("vscode+view-B"));
        assert_eq!(agent_at(7000), Some("vscode+view-B"));
    }
}
