//! Notifications projection — derives active notifications from the
//! events table by running a small set of Rule implementations over
//! per-tile event streams. See spec
//! 2026-04-24-notifications-projection-design.md.

pub mod notification;
pub mod rule;
pub mod rules;

use crate::events::store::query::EventRow;
use crate::events::tiles;
use crate::events::tiles::derive::{derive, parse_view_visible_change, VscodeAttribution, VscodeRecentFocus};
use crate::events::tiles::tile::{id_for as tile_id_for, Tile};
use notification::Notification;
use rusqlite::Connection;
use std::collections::HashMap;

/// Compute the notifications projection over events with
/// received_at >= since_ms. Pure function over the events table —
/// no caching, no incremental state.
///
/// Returns notifications sorted by triggered_at_ms DESC.
pub fn compute(conn: &Connection, since_ms: i64) -> rusqlite::Result<Vec<Notification>> {
    let tiles_list = tiles::compute(conn, since_ms)?;
    let events = tiles::fetch_since(conn, since_ms)?;
    let buckets = bucket_events_by_tile(&events);
    let now_ms = unix_now_ms();

    const EMPTY: &[&EventRow] = &[];
    let mut out = Vec::new();
    for tile in &tiles_list {
        let tile_events: &[&EventRow] = buckets
            .get(&tile.id)
            .map(|v| v.as_slice())
            .unwrap_or(EMPTY);
        for rule in rules::ALL_RULES {
            if let Some(body) = rule.evaluate(tile, tile_events, now_ms) {
                out.push(assemble(tile, *rule, body));
            }
        }
    }

    out.sort_by(|a, b| b.triggered_at_ms.cmp(&a.triggered_at_ms));
    Ok(out)
}

/// Single-pass walk: maintain a rolling VS Code "currently visible view
/// per window" map (same pattern as tiles::walk_and_derive). For each
/// row that derives a tile identity, push a &EventRow into that tile's
/// bucket. Events that don't derive (no context, malformed, or view
/// visible=false) are dropped.
fn bucket_events_by_tile<'a>(events: &'a [EventRow]) -> HashMap<String, Vec<&'a EventRow>> {
    use crate::events::tiles::derive::parse_vscode_focus_signal;
    let mut attr = VscodeAttribution::new();
    let mut recent_focus = VscodeRecentFocus::default();
    let mut buckets: HashMap<String, Vec<&EventRow>> = HashMap::new();
    for row in events {
        if let Some((window_pid, view, visible)) = parse_view_visible_change(row) {
            if visible {
                attr.insert(window_pid, view);
            } else {
                attr.remove(&window_pid);
            }
        }
        if let Some((window_pid, workspace_root, ts_ms)) = parse_vscode_focus_signal(row) {
            recent_focus = VscodeRecentFocus {
                ts_ms: Some(ts_ms),
                window_pid: Some(window_pid),
                workspace_root: Some(workspace_root),
            };
        }
        if let Some(d) = derive(row, &attr, &recent_focus) {
            let tile_id = tile_id_for(&d.agent, &d.project_anchor, &d.surface_token);
            buckets.entry(tile_id).or_default().push(row);
        }
    }
    buckets
}

fn assemble(tile: &Tile, rule: &dyn rule::Rule, body: rule::NotificationBody) -> Notification {
    Notification {
        id: notification::id_for(rule.id(), &tile.id),
        rule_id: rule.id().to_string(),
        tile_id: tile.id.clone(),
        agent: tile.agent.clone(),
        project_label: tile.project_label.clone(),
        severity: rule.severity(),
        message: body.message,
        trigger_event_id: body.trigger_event_id,
        triggered_at_ms: body.triggered_at_ms,
        focus_uri: tile.focus_uri.clone(),
    }
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::schema::run_migrations;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    /// Open a private (non-global) connection for testing, mirroring the
    /// approach in store/mod.rs to avoid OnceLock double-init panics.
    fn open_test_conn() -> (NamedTempFile, Connection) {
        let f = NamedTempFile::new().unwrap();
        let conn = Connection::open(f.path()).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        run_migrations(&conn).unwrap();
        (f, conn)
    }

    fn insert_event(
        conn: &rusqlite::Connection,
        event_id: &str,
        event_type: &str,
        source: &str,
        event_ts: i64,
        agent: &str,
        workspace_root: &str,
        subapp_session: &str,
        subapp_pane: &str,
        payload: serde_json::Value,
    ) {
        let context = serde_json::json!({
            "agent": agent,
            "workspace_root": workspace_root,
            "subapplication": {
                "kind": "tmux",
                "session": subapp_session,
                "pane": subapp_pane,
            }
        });
        conn.execute(
            "INSERT INTO events (received_at, event_id, schema_version, event_ts, seq,
                host, os_user, device_id, source, source_pid, event_type,
                session_id, project, correlation, context, payload)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                event_ts,
                event_id,
                1,
                event_ts,
                0,
                "h",
                "u",
                "d",
                source,
                1,
                event_type,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Some(context.to_string()),
                Some(payload.to_string()),
            ],
        )
        .unwrap();
    }

    #[test]
    fn compute_with_empty_events_returns_empty() {
        let (_f, conn) = open_test_conn();
        let out = compute(&conn, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn compute_assembles_notifications_from_tiles_and_rules() {
        let (_f, conn) = open_test_conn();

        // Tile "zestful" — latest event is turn.completed → Rule A fires.
        insert_event(
            &conn,
            "evt-1",
            "turn.prompt_submitted",
            "claude-code",
            1000,
            "claude-code",
            "/x/zestful",
            "z",
            "%0",
            serde_json::json!({}),
        );
        insert_event(
            &conn,
            "evt-2",
            "turn.completed",
            "claude-code",
            2000,
            "claude-code",
            "/x/zestful",
            "z",
            "%0",
            serde_json::json!({}),
        );

        // Tile "other" — latest event is tool.invoked → Rule A does NOT fire.
        insert_event(
            &conn,
            "evt-3",
            "tool.invoked",
            "claude-code",
            3000,
            "claude-code",
            "/x/other",
            "o",
            "%1",
            serde_json::json!({}),
        );

        let notifications = compute(&conn, 0).unwrap();
        assert_eq!(notifications.len(), 1, "got {:?}", notifications);
        let n = &notifications[0];
        assert_eq!(n.rule_id, "agent_completed");
        assert_eq!(n.agent, "claude-code");
        assert_eq!(n.project_label.as_deref(), Some("zestful"));
        assert_eq!(n.trigger_event_id, "evt-2");
        assert_eq!(n.triggered_at_ms, 2000);
    }

    #[test]
    fn compute_result_is_sorted_by_triggered_at_desc() {
        let (_f, conn) = open_test_conn();

        insert_event(
            &conn,
            "evt-1",
            "turn.completed",
            "claude-code",
            1000,
            "claude-code",
            "/x/alpha",
            "a",
            "%0",
            serde_json::json!({}),
        );
        insert_event(
            &conn,
            "evt-2",
            "turn.completed",
            "claude-code",
            5000,
            "claude-code",
            "/x/beta",
            "b",
            "%1",
            serde_json::json!({}),
        );

        let notifications = compute(&conn, 0).unwrap();
        assert_eq!(notifications.len(), 2);
        assert!(
            notifications[0].triggered_at_ms >= notifications[1].triggered_at_ms,
            "not DESC: {:?}",
            notifications.iter().map(|n| n.triggered_at_ms).collect::<Vec<_>>()
        );
        assert_eq!(notifications[0].trigger_event_id, "evt-2");
        assert_eq!(notifications[1].trigger_event_id, "evt-1");
    }

    #[test]
    fn compute_id_is_stable_across_calls() {
        let (_f, conn) = open_test_conn();

        insert_event(
            &conn,
            "evt-1",
            "turn.completed",
            "claude-code",
            1000,
            "claude-code",
            "/x/zestful",
            "z",
            "%0",
            serde_json::json!({}),
        );

        let a = compute(&conn, 0).unwrap();
        let b = compute(&conn, 0).unwrap();
        assert_eq!(a[0].id, b[0].id);
    }
}
