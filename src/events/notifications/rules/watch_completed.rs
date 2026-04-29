//! Rule D: watch_completed — fires when the tile's most recent event is
//! `watch.completed` (emitted by `zestful watch` on command exit).
//! Severity is Urgent on nonzero exit, Warn on zero. Push fires whenever
//! severity is non-Info (always for this rule, since both Warn and Urgent
//! qualify).

use crate::events::notifications::rule::{NotificationBody, Rule, Severity};
use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;

pub struct WatchCompleted;

impl Rule for WatchCompleted {
    fn id(&self) -> &'static str {
        "watch_completed"
    }
    fn evaluate(
        &self,
        _tile: &Tile,
        tile_events: &[&EventRow],
        _now_ms: i64,
    ) -> Option<NotificationBody> {
        let latest = tile_events.last()?;
        if latest.event_type != "watch.completed" {
            return None;
        }

        let payload = latest.payload.as_ref()?;
        let exit_code = payload.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(0);
        let command = payload.get("command").and_then(|v| v.as_str()).unwrap_or("command");

        let severity = if exit_code != 0 { Severity::Urgent } else { Severity::Warn };

        let message = payload
            .get("message")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                if exit_code != 0 {
                    format!("{} failed (exit {})", command, exit_code)
                } else {
                    format!("{} finished", command)
                }
            });

        Some(NotificationBody {
            message,
            trigger_event_id: latest.event_id.clone(),
            triggered_at_ms: latest.event_ts,
            severity,
            push: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tile_fixture() -> Tile {
        Tile {
            id: "tile_test".to_string(),
            agent: "watch:npm".to_string(),
            project_anchor: Some("/x/zestful".to_string()),
            project_label: Some("zestful".to_string()),
            surface_kind: "cli".to_string(),
            surface_token: "tmux:z/pane:%0".to_string(),
            surface_label: "tmux z → pane %0".to_string(),
            first_seen_at: 0,
            last_seen_at: 0,
            event_count: 0,
            latest_event_type: "".to_string(),
            focus_uri: None,
        }
    }

    fn ev(id: i64, event_type: &str, event_ts: i64, payload: serde_json::Value) -> EventRow {
        EventRow {
            id,
            received_at: event_ts,
            event_id: format!("evt-{}", id),
            event_type: event_type.to_string(),
            source: "cli".to_string(),
            session_id: None,
            project: None,
            host: "h".to_string(),
            os_user: "u".to_string(),
            device_id: "d".to_string(),
            event_ts,
            seq: 0,
            source_pid: 1,
            schema_version: 1,
            correlation: None,
            context: Some(json!({})),
            payload: Some(payload),
        }
    }

    #[test]
    fn fires_urgent_on_nonzero_exit() {
        let tile = tile_fixture();
        let e1 = ev(1, "watch.completed", 1000, json!({"command": "npm", "exit_code": 1}));
        let refs = vec![&e1];
        let body = WatchCompleted.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.severity, Severity::Urgent);
        assert_eq!(body.push, true);
        assert_eq!(body.message, "npm failed (exit 1)");
    }

    #[test]
    fn fires_warn_on_zero_exit() {
        let tile = tile_fixture();
        let e1 = ev(1, "watch.completed", 1000, json!({"command": "true", "exit_code": 0}));
        let refs = vec![&e1];
        let body = WatchCompleted.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.severity, Severity::Warn);
        assert_eq!(body.message, "true finished");
    }

    #[test]
    fn does_not_fire_when_latest_event_is_other_type() {
        let tile = tile_fixture();
        let e1 = ev(1, "watch.completed", 1000, json!({"command": "x", "exit_code": 0}));
        let e2 = ev(2, "agent.notified", 2000, json!({"kind": "notification"}));
        let refs = vec![&e1, &e2];
        assert_eq!(WatchCompleted.evaluate(&tile, &refs, 3000), None);
    }

    #[test]
    fn explicit_message_overrides_synthesized() {
        let tile = tile_fixture();
        let e1 = ev(1, "watch.completed", 1000,
            json!({"command": "npm", "exit_code": 1, "message": "custom failure note"}));
        let refs = vec![&e1];
        let body = WatchCompleted.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.message, "custom failure note");
    }

    #[test]
    fn returns_none_when_no_events() {
        let tile = tile_fixture();
        let refs: Vec<&EventRow> = vec![];
        assert_eq!(WatchCompleted.evaluate(&tile, &refs, 1000), None);
    }
}
