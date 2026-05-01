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

/// Post-pass: for each tile, find its "active session" (the most recent
/// turn.metrics event whose session_id maps to the tile via the most
/// recent surface-attributed event for that session_id) and attach
/// aggregated metrics. Tiles whose active session has no turn.metrics
/// remain `metrics: None`.
///
/// `now_ms` is passed in (rather than read from the clock) so the
/// burn_rate calculation is deterministic in tests.
pub fn enrich_with_metrics(
    conn: &Connection,
    tiles: &mut [tile::Tile],
    since_ms: i64,
    now_ms: i64,
) -> rusqlite::Result<()> {
    use crate::events::payload::TurnTokens;
    use std::collections::HashMap;

    // 1) Build session_id → tile_id map from the most recent
    // surface-attributed event for each session_id in the window.
    let mut stmt = conn.prepare(
        "SELECT session_id, context
         FROM events
         WHERE event_ts >= ? AND session_id IS NOT NULL AND context IS NOT NULL
         ORDER BY event_ts DESC, id DESC"
    )?;
    let mut session_to_tile: HashMap<String, String> = HashMap::new();
    let rows = stmt.query_map([since_ms], |row| {
        let s: String = row.get(0)?;
        let c: String = row.get(1)?;
        Ok((s, c))
    })?;
    let tiles_by_id: HashMap<String, &tile::Tile> = tiles.iter()
        .map(|t| (t.id.clone(), &*t)).collect();
    for r in rows {
        let (sid, ctx_str) = r?;
        if session_to_tile.contains_key(&sid) { continue; }
        let ctx: serde_json::Value = match serde_json::from_str(&ctx_str) {
            Ok(v) => v, Err(_) => continue,
        };
        let agent = match ctx.get("agent").and_then(|v| v.as_str()) {
            Some(a) => a, None => continue,
        };
        let surface = ctx.get("surface_token").and_then(|v| v.as_str())
            .unwrap_or("");
        for t in tiles_by_id.values() {
            if t.agent == agent && t.surface_token == surface {
                session_to_tile.insert(sid.clone(), t.id.clone());
                break;
            }
        }
    }
    drop(stmt);

    // 2) Aggregate turn.metrics per session.
    struct Agg {
        last_ts: i64,
        last_ratio: Option<f64>,
        last_used_tokens: u64,
        last_max_tokens: Option<u64>,
        last_model: String,
        cost_total: f64,
        cost_total_known: bool,
        cost_last_60min: f64,
        tokens_in: u64, tokens_out: u64,
        tokens_cache_read: u64, tokens_cache_write: u64, tokens_reasoning: u64,
        first_ts: i64,
    }
    let mut per_session: HashMap<String, Agg> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT session_id, event_ts, payload
         FROM events
         WHERE event_type = 'turn.metrics'
           AND event_ts >= ? AND session_id IS NOT NULL"
    )?;
    let rows = stmt.query_map([since_ms], |row| {
        let s: String = row.get(0)?;
        let ts: i64 = row.get(1)?;
        let p: Option<String> = row.get(2)?;
        Ok((s, ts, p))
    })?;
    for r in rows {
        let (sid, ts, payload_str) = r?;
        let p: serde_json::Value = match payload_str.and_then(|s| serde_json::from_str(&s).ok()) {
            Some(v) => v, None => continue,
        };
        let entry = per_session.entry(sid).or_insert(Agg {
            last_ts: 0, last_ratio: None,
            last_used_tokens: 0, last_max_tokens: None,
            last_model: String::new(),
            cost_total: 0.0, cost_total_known: true, cost_last_60min: 0.0,
            tokens_in: 0, tokens_out: 0,
            tokens_cache_read: 0, tokens_cache_write: 0, tokens_reasoning: 0,
            first_ts: i64::MAX,
        });
        if ts < entry.first_ts { entry.first_ts = ts; }
        if ts >= entry.last_ts {
            entry.last_ts = ts;
            let ctx = p.get("context");
            entry.last_ratio = ctx.and_then(|c| c.get("ratio")).and_then(|v| v.as_f64());
            entry.last_used_tokens = ctx.and_then(|c| c.get("used_tokens"))
                                        .and_then(|v| v.as_u64()).unwrap_or(0);
            entry.last_max_tokens = ctx.and_then(|c| c.get("max_tokens"))
                                       .and_then(|v| v.as_u64());
            entry.last_model = p.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
        }
        match p.get("cost_estimate_usd").and_then(|v| v.as_f64()) {
            Some(c) => {
                entry.cost_total += c;
                if ts >= now_ms - 3_600_000 { entry.cost_last_60min += c; }
            }
            None => entry.cost_total_known = false,
        }
        if let Some(t) = p.get("tokens").and_then(|v| v.as_object()) {
            entry.tokens_in           += t.get("input")      .and_then(|v| v.as_u64()).unwrap_or(0);
            entry.tokens_out          += t.get("output")     .and_then(|v| v.as_u64()).unwrap_or(0);
            entry.tokens_cache_read   += t.get("cache_read") .and_then(|v| v.as_u64()).unwrap_or(0);
            entry.tokens_cache_write  += t.get("cache_write").and_then(|v| v.as_u64()).unwrap_or(0);
            entry.tokens_reasoning    += t.get("reasoning")  .and_then(|v| v.as_u64()).unwrap_or(0);
        }
    }
    drop(stmt);

    // 3) For each tile, pick the most-recent session attached and attach metrics.
    let mut tile_to_session: HashMap<String, (String, i64)> = HashMap::new();
    for (sid, tile_id) in &session_to_tile {
        let Some(agg) = per_session.get(sid) else { continue; };
        tile_to_session.entry(tile_id.clone())
            .and_modify(|slot| {
                if agg.last_ts > slot.1 { *slot = (sid.clone(), agg.last_ts); }
            })
            .or_insert((sid.clone(), agg.last_ts));
    }

    for tile in tiles.iter_mut() {
        let Some((sid, _)) = tile_to_session.get(&tile.id) else { continue; };
        let Some(agg) = per_session.get(sid) else { continue; };
        let session_cost_usd = if agg.cost_total_known { Some(agg.cost_total) } else { None };
        let cache_hit_denom = agg.tokens_in + agg.tokens_cache_read;
        let cache_hit_pct = if cache_hit_denom > 0 {
            Some(agg.tokens_cache_read as f64 / cache_hit_denom as f64)
        } else { None };
        let elapsed_min = ((now_ms - agg.first_ts).max(0) / 60_000).max(1) as f64;
        let burn_rate = if agg.cost_last_60min > 0.0 {
            Some(agg.cost_last_60min / elapsed_min.min(60.0) * 60.0)
        } else { None };
        tile.metrics = Some(tile::TileMetrics {
            model: agg.last_model.clone(),
            session_id: sid.clone(),
            context_pct: agg.last_ratio,
            context_used_tokens: agg.last_used_tokens,
            context_max_tokens: agg.last_max_tokens,
            session_cost_usd,
            cache_hit_pct,
            burn_rate_usd_hr: burn_rate,
            tokens: TurnTokens {
                input: agg.tokens_in,
                output: agg.tokens_out,
                cache_read: agg.tokens_cache_read,
                cache_write: agg.tokens_cache_write,
                reasoning: agg.tokens_reasoning,
            },
        });
    }

    Ok(())
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

    // Helper for enrich tests: build a Tile directly so we test enrich
    // in isolation from the compute() derivation pipeline (which has its
    // own coverage).
    fn synth_tile(agent: &str, surface_token: &str) -> crate::events::tiles::tile::Tile {
        use crate::events::tiles::tile::{Tile, id_for};
        Tile {
            id: id_for(agent, "/x/zestful", surface_token),
            agent: agent.to_string(),
            project_anchor: Some("/x/zestful".to_string()),
            project_label: Some("zestful".to_string()),
            surface_kind: "cli".to_string(),
            surface_token: surface_token.to_string(),
            surface_label: surface_token.to_string(),
            first_seen_at: 0, last_seen_at: 0,
            event_count: 0, latest_event_type: "x".into(),
            focus_uri: None, metrics: None,
        }
    }

    #[test]
    fn enrich_attaches_metrics_when_session_has_surface_attributed_event() {
        let conn = Connection::open_in_memory().unwrap();
        crate::events::store::schema::run_migrations(&conn).unwrap();

        let now = 1_761_830_000_000i64;

        // 1. Surface-attributed event maps session → tile.
        conn.execute(
            "INSERT INTO events (
                received_at, event_id, schema_version, event_ts, seq,
                host, os_user, device_id, source, source_pid,
                event_type, session_id, project, correlation, context, payload
            ) VALUES (?,?,1,?,0,'h','u','d','hook',1,
                      'turn.completed',?,'/x/zestful',?,?,?)",
            rusqlite::params![
                now - 5000, "h1", now - 5000, "s1",
                json!({ "session_id": "s1" }).to_string(),
                json!({ "agent": "claude-code", "surface_token": "tmux:z/pane:%0" }).to_string(),
                json!({}).to_string(),
            ],
        ).unwrap();

        // 2. turn.metrics for the same session.
        conn.execute(
            "INSERT INTO events (
                received_at, event_id, schema_version, event_ts, seq,
                host, os_user, device_id, source, source_pid,
                event_type, session_id, project, correlation, context, payload
            ) VALUES (?,?,1,?,0,'h','u','d','agent-scraper',1,
                      'turn.metrics',?,NULL,?,?,?)",
            rusqlite::params![
                now - 1000, "m1", now - 1000, "s1",
                json!({ "session_id": "s1" }).to_string(),
                json!({ "agent": "claude-code", "model": "claude-3-5-sonnet-20241022" }).to_string(),
                json!({
                    "model": "claude-3-5-sonnet-20241022",
                    "tokens": { "input": 1000, "output": 200, "cache_read": 500,
                                "cache_write": 0, "reasoning": 0 },
                    "context": { "used_tokens": 1000, "max_tokens": 200000, "ratio": 0.005 },
                    "cost_estimate_usd": 0.10,
                    "message_count": 1
                }).to_string(),
            ],
        ).unwrap();

        let mut tiles = vec![synth_tile("claude-code", "tmux:z/pane:%0")];
        enrich_with_metrics(&conn, &mut tiles, now - 24 * 3_600_000, now).unwrap();

        let m = tiles[0].metrics.as_ref().expect("metrics attached");
        assert_eq!(m.session_id, "s1");
        assert_eq!(m.model, "claude-3-5-sonnet-20241022");
        assert!((m.context_pct.unwrap() - 0.005).abs() < 1e-9);
        assert_eq!(m.context_used_tokens, 1000);
        assert_eq!(m.context_max_tokens, Some(200_000));
        assert!((m.session_cost_usd.unwrap() - 0.10).abs() < 1e-9);
        assert!((m.cache_hit_pct.unwrap() - (500.0 / 1500.0)).abs() < 1e-9);
        assert_eq!(m.tokens.input, 1000);
    }

    #[test]
    fn enrich_leaves_metrics_none_when_session_has_no_surface_event() {
        let conn = Connection::open_in_memory().unwrap();
        crate::events::store::schema::run_migrations(&conn).unwrap();

        let now = 1_761_830_000_000i64;
        conn.execute(
            "INSERT INTO events (
                received_at, event_id, schema_version, event_ts, seq,
                host, os_user, device_id, source, source_pid,
                event_type, session_id, project, correlation, context, payload
            ) VALUES (?,?,1,?,0,'h','u','d','agent-scraper',1,
                      'turn.metrics',?,NULL,?,?,?)",
            rusqlite::params![
                now - 1000, "m1", now - 1000, "s_orphan",
                json!({ "session_id": "s_orphan" }).to_string(),
                json!({ "agent": "claude-code", "model": "claude-3-5-sonnet-20241022" }).to_string(),
                json!({
                    "model": "claude-3-5-sonnet-20241022",
                    "tokens": { "input": 100, "output": 0, "cache_read": 0,
                                "cache_write": 0, "reasoning": 0 },
                    "context": { "used_tokens": 100, "max_tokens": 200000, "ratio": 0.0005 },
                    "cost_estimate_usd": 0.01,
                    "message_count": 1
                }).to_string(),
            ],
        ).unwrap();

        let mut tiles = vec![synth_tile("claude-code", "tmux:z/pane:%0")];
        enrich_with_metrics(&conn, &mut tiles, now - 24 * 3_600_000, now).unwrap();
        assert!(tiles[0].metrics.is_none(),
                "expected metrics: None when no surface event maps the session");
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
