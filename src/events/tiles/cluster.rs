//! Group DerivedRows by identity and aggregate per-tile fields.

use crate::events::tiles::derive::DerivedRow;
use crate::events::tiles::{surfaces, tile};
use std::collections::HashMap;

/// Group derived rows by (agent, project_anchor, surface_token), build
/// one Tile per group with aggregates. Output sorted by last_seen_at
/// DESC.
pub fn group(rows: &[DerivedRow]) -> Vec<tile::Tile> {
    // First pass: bucket by identity tuple.
    let mut buckets: HashMap<(String, String, String), Vec<&DerivedRow>> = HashMap::new();
    for r in rows {
        let key = (r.agent.clone(), r.project_anchor.clone(), r.surface_token.clone());
        buckets.entry(key).or_default().push(r);
    }

    // Second pass: build a Tile per bucket.
    let mut tiles: Vec<tile::Tile> = buckets
        .into_iter()
        .map(|((agent, project_anchor, surface_token), bucket)| {
            // unwrap()s below are safe: bucket is non-empty by construction
            // (HashMap entry was created on first push() of this row).
            let first_seen_at = bucket.iter().map(|r| r.received_at).min().unwrap();
            // last_seen_at represents agent activity, not user activity.
            // `focus.acknowledged` events are user clicks (the Mac app emits
            // them when the user clicks the overlay or Focus button) and
            // shouldn't bump the tile's freshness timer used for the
            // Working/Ready/Idle UI state machine. Fall back to the unfiltered
            // max only if the bucket contains nothing else (a rare edge case
            // where the only contributing event is a focus.acknowledged).
            let last_seen_at = bucket
                .iter()
                .filter(|r| r.event_type != "focus.acknowledged")
                .map(|r| r.received_at)
                .max()
                .unwrap_or_else(|| bucket.iter().map(|r| r.received_at).max().unwrap());
            let event_count = bucket.len() as i64;
            let latest = bucket.iter().max_by_key(|r| r.received_at).unwrap();
            let latest_event_type = latest.event_type.clone();
            // focus_uri: pull from the most recent row that HAD one.
            let focus_uri = bucket
                .iter()
                .filter(|r| r.focus_uri.is_some())
                .max_by_key(|r| r.received_at)
                .and_then(|r| r.focus_uri.clone());
            // Invariant: all rows in a bucket share surface_kind, because
            // surface_token values are produced by per-source code paths
            // that don't overlap (tmux tokens only from CLI, vscode-window
            // tokens only from vscode, etc.). debug_assert! catches a
            // future regression cheaply.
            debug_assert!(
                bucket.iter().all(|r| r.surface_kind == bucket[0].surface_kind),
                "surface_kind mismatch in bucket for surface_token={}", surface_token
            );
            let surface_kind = bucket[0].surface_kind.clone();
            let id = tile::id_for(&agent, &project_anchor, &surface_token);
            // Extract the app slug from the focus URI (e.g. "terminal" from
            // "workspace://terminal/window:282/tab:1") so surface_label can
            // distinguish Terminal.app from iTerm2 and other terminal emulators.
            let app_slug: Option<String> = focus_uri
                .as_deref()
                .and_then(|u| u.strip_prefix("workspace://"))
                .and_then(|s| s.split('/').next())
                .map(|s| s.to_string());
            tile::Tile {
                id,
                agent: agent.clone(),
                project_anchor: Some(project_anchor.clone()),
                project_label: surfaces::project_label(Some(&project_anchor)),
                surface_kind: surface_kind.clone(),
                surface_token: surface_token.clone(),
                surface_label: surfaces::surface_label(&surface_kind, &surface_token, app_slug.as_deref()),
                first_seen_at,
                last_seen_at,
                event_count,
                latest_event_type,
                focus_uri,
            }
        })
        .collect();

    // Sort by last_seen_at DESC, with id as a stable tiebreaker.
    tiles.sort_by(|a, b| {
        b.last_seen_at
            .cmp(&a.last_seen_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dr(agent: &str, anchor: &str, surface: &str, kind: &str, ts: i64, et: &str) -> DerivedRow {
        DerivedRow {
            agent: agent.to_string(),
            project_anchor: anchor.to_string(),
            surface_kind: kind.to_string(),
            surface_token: surface.to_string(),
            received_at: ts,
            event_type: et.to_string(),
            focus_uri: None,
        }
    }

    #[test]
    fn group_empty_returns_empty() {
        let tiles = group(&[]);
        assert!(tiles.is_empty());
    }

    #[test]
    fn group_single_row_produces_one_tile() {
        let rows = vec![dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 1000, "turn.completed")];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].agent, "claude-code");
        assert_eq!(tiles[0].project_anchor.as_deref(), Some("/x"));
        assert_eq!(tiles[0].surface_token, "tmux:z/pane:%0");
        assert_eq!(tiles[0].first_seen_at, 1000);
        assert_eq!(tiles[0].last_seen_at, 1000);
        assert_eq!(tiles[0].event_count, 1);
        assert_eq!(tiles[0].latest_event_type, "turn.completed");
    }

    #[test]
    fn group_subdir_changes_collapse_via_anchor() {
        // Three events for the same agent on /x, all anchored to /x via
        // the env var (project_anchor is /x for all of them) → one tile.
        let rows = vec![
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 1000, "turn.prompt_submitted"),
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 2000, "tool.invoked"),
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 3000, "turn.completed"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].event_count, 3);
        assert_eq!(tiles[0].first_seen_at, 1000);
        assert_eq!(tiles[0].last_seen_at, 3000);
        assert_eq!(tiles[0].latest_event_type, "turn.completed");
    }

    #[test]
    fn group_concurrent_panes_split_into_two_tiles() {
        let rows = vec![
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 1000, "turn.completed"),
            dr("claude-code", "/x", "tmux:z/pane:%1", "cli", 1500, "turn.completed"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 2);
    }

    #[test]
    fn group_different_agents_split() {
        let rows = vec![
            dr("claude-code",  "/x", "tmux:z/pane:%0", "cli", 1000, "turn.completed"),
            dr("codex-cli",    "/x", "tmux:z/pane:%0", "cli", 2000, "turn.completed"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 2);
    }

    #[test]
    fn group_different_projects_split() {
        let rows = vec![
            dr("claude-code", "/x/Fubar",  "tmux:z/pane:%0", "cli", 1000, "turn.completed"),
            dr("claude-code", "/x/Wibble", "tmux:z/pane:%0", "cli", 2000, "turn.completed"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 2);
    }

    #[test]
    fn group_sorts_by_last_seen_at_desc() {
        let rows = vec![
            dr("a", "/x", "s1", "cli", 1000, "e"),
            dr("b", "/x", "s2", "cli", 3000, "e"),
            dr("c", "/x", "s3", "cli", 2000, "e"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[0].last_seen_at, 3000);
        assert_eq!(tiles[1].last_seen_at, 2000);
        assert_eq!(tiles[2].last_seen_at, 1000);
    }

    #[test]
    fn group_excludes_focus_acknowledged_from_last_seen_at() {
        // A focus.acknowledged event represents a user click, not agent
        // activity. It must remain the latest_event_type so the projection
        // rule engine can dismiss the open notification, but it must NOT
        // bump last_seen_at — the tile's freshness timer drives the
        // Working/Ready/Idle UI states and shouldn't reset on user clicks.
        let rows = vec![
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 1000, "turn.completed"),
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 2000, "agent.notified"),
            dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 3000, "focus.acknowledged"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 1);
        // last_seen_at reflects the newest non-focus.acknowledged event.
        assert_eq!(tiles[0].last_seen_at, 2000);
        // latest_event_type is still focus.acknowledged so the projection
        // rule engine sees the dismissal.
        assert_eq!(tiles[0].latest_event_type, "focus.acknowledged");
        // event_count includes all events.
        assert_eq!(tiles[0].event_count, 3);
    }

    #[test]
    fn group_falls_back_to_focus_acknowledged_only_bucket() {
        // Edge case: if the only contributing event is focus.acknowledged
        // (shouldn't happen in practice — there must be a trigger event for
        // a user to click — but is a logical fallback), don't panic.
        let rows = vec![
            dr("a", "/x", "s", "cli", 1000, "focus.acknowledged"),
        ];
        let tiles = group(&rows);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].last_seen_at, 1000);
        assert_eq!(tiles[0].latest_event_type, "focus.acknowledged");
    }

    #[test]
    fn group_aggregates_focus_uri_from_latest_with_one() {
        let mut row1 = dr("a", "/x", "s", "cli", 1000, "e");
        row1.focus_uri = Some("uri-1".to_string());
        let mut row2 = dr("a", "/x", "s", "cli", 2000, "e");
        row2.focus_uri = Some("uri-2".to_string());
        let row3 = dr("a", "/x", "s", "cli", 3000, "e");  // no focus_uri
        let tiles = group(&[row1, row2, row3]);
        assert_eq!(tiles.len(), 1);
        // focus_uri comes from the LATEST row that HAD one — so uri-2,
        // not uri-1 (older) and not None (latest row had none).
        assert_eq!(tiles[0].focus_uri.as_deref(), Some("uri-2"));
    }

    #[test]
    fn group_tile_id_is_deterministic() {
        let rows1 = vec![dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 1000, "e")];
        let rows2 = vec![dr("claude-code", "/x", "tmux:z/pane:%0", "cli", 2000, "e")];
        let id1 = group(&rows1)[0].id.clone();
        let id2 = group(&rows2)[0].id.clone();
        assert_eq!(id1, id2);
    }

    #[test]
    fn group_populates_labels() {
        let rows = vec![dr("claude-code", "/x/Fubar", "tmux:z/pane:%0", "cli", 1000, "e")];
        let tiles = group(&rows);
        assert_eq!(tiles[0].project_label.as_deref(), Some("Fubar"));
        assert!(tiles[0].surface_label.contains("tmux [z:"), "label = {}", tiles[0].surface_label);
    }
}
