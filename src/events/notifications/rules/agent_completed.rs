//! Rule A: agent_completed — fires when the tile's most recent event
//! is `turn.completed`.

use crate::events::notifications::rule::{NotificationBody, Rule, Severity};
use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;

pub struct AgentCompleted;

impl Rule for AgentCompleted {
    fn id(&self) -> &'static str {
        "agent_completed"
    }
    fn evaluate(
        &self,
        tile: &Tile,
        tile_events: &[&EventRow],
        _now_ms: i64,
    ) -> Option<NotificationBody> {
        let latest = tile_events.last()?;
        if latest.event_type != "turn.completed" {
            return None;
        }
        let message = match tile.project_label.as_deref() {
            Some(p) => format!("{} completed on {}", tile.agent, p),
            None => format!("{} completed", tile.agent),
        };
        Some(NotificationBody {
            message,
            trigger_event_id: latest.event_id.clone(),
            triggered_at_ms: latest.event_ts,
            severity: Severity::Warn,
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
            agent: "claude-code".to_string(),
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
            metrics: None,
        }
    }

    fn ev(id: i64, event_type: &str, event_ts: i64) -> EventRow {
        EventRow {
            id,
            received_at: event_ts,
            event_id: format!("evt-{}", id),
            event_type: event_type.to_string(),
            source: "claude-code".to_string(),
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
            payload: Some(json!({})),
        }
    }

    #[test]
    fn fires_when_latest_event_is_turn_completed() {
        let tile = tile_fixture();
        let e1 = ev(1, "turn.prompt_submitted", 1000);
        let e2 = ev(2, "turn.completed", 2000);
        let refs = vec![&e1, &e2];
        let body = AgentCompleted
            .evaluate(&tile, &refs, 3000)
            .expect("expected Some(body)");
        assert_eq!(body.message, "claude-code completed on zestful");
        assert_eq!(body.trigger_event_id, "evt-2");
        assert_eq!(body.triggered_at_ms, 2000);
        assert_eq!(body.severity, Severity::Warn);
        assert_eq!(body.push, true);
    }

    #[test]
    fn does_not_fire_when_newer_prompt_submitted_follows() {
        let tile = tile_fixture();
        let e1 = ev(1, "turn.completed", 1000);
        let e2 = ev(2, "turn.prompt_submitted", 2000);
        let refs = vec![&e1, &e2];
        assert_eq!(AgentCompleted.evaluate(&tile, &refs, 3000), None);
    }

    #[test]
    fn does_not_fire_when_latest_event_is_tool_invoked() {
        let tile = tile_fixture();
        let e1 = ev(1, "turn.completed", 1000);
        let e2 = ev(2, "tool.invoked", 2000);
        let refs = vec![&e1, &e2];
        assert_eq!(AgentCompleted.evaluate(&tile, &refs, 3000), None);
    }

    #[test]
    fn message_omits_on_project_when_project_label_is_none() {
        let mut tile = tile_fixture();
        tile.project_label = None;
        let e1 = ev(1, "turn.completed", 1000);
        let refs = vec![&e1];
        let body = AgentCompleted
            .evaluate(&tile, &refs, 2000)
            .expect("expected Some(body)");
        assert_eq!(body.message, "claude-code completed");
        assert_eq!(body.severity, Severity::Warn);
        assert_eq!(body.push, true);
    }
}
