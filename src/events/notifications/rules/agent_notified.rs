//! Rule C: agent_notified — fires when the tile's most recent event is
//! `agent.notified`. Covers Claude's explicit Notification hook, the
//! chrome extension's "AI response arrived" signal, and future richer
//! attention signals.

use crate::events::notifications::rule::{NotificationBody, Rule, Severity};
use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;

pub struct AgentNotified;

impl Rule for AgentNotified {
    fn id(&self) -> &'static str {
        "agent_notified"
    }
    fn evaluate(
        &self,
        tile: &Tile,
        tile_events: &[&EventRow],
        _now_ms: i64,
    ) -> Option<NotificationBody> {
        let latest = tile_events.last()?;
        if latest.event_type != "agent.notified" {
            return None;
        }
        let payload = latest.payload.as_ref();
        let message = payload
            .and_then(|p| p.get("message"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("{} wants attention", tile.agent));

        let severity_hint = payload
            .and_then(|p| p.get("severity_hint"))
            .and_then(|v| v.as_str())
            .and_then(|s| match s {
                "info" => Some(Severity::Info),
                "warn" => Some(Severity::Warn),
                "urgent" => Some(Severity::Urgent),
                _ => None,
            });
        let severity = severity_hint.unwrap_or(Severity::Info);

        let push_hint = payload
            .and_then(|p| p.get("push_hint"))
            .and_then(|v| v.as_bool());
        let push = push_hint.unwrap_or(severity != Severity::Info);

        Some(NotificationBody {
            message,
            trigger_event_id: latest.event_id.clone(),
            triggered_at_ms: latest.event_ts,
            severity,
            push,
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

    fn ev(id: i64, event_type: &str, event_ts: i64, payload: serde_json::Value) -> EventRow {
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
            payload: Some(payload),
        }
    }

    #[test]
    fn fires_when_latest_event_is_agent_notified() {
        let tile = tile_fixture();
        let e1 = ev(
            1,
            "agent.notified",
            1000,
            json!({ "kind": "notification", "message": "Claude asks: continue?" }),
        );
        let refs = vec![&e1];
        let body = AgentNotified
            .evaluate(&tile, &refs, 2000)
            .expect("expected Some(body)");
        assert_eq!(body.message, "Claude asks: continue?");
        assert_eq!(body.trigger_event_id, "evt-1");
        assert_eq!(body.triggered_at_ms, 1000);
        assert_eq!(body.severity, Severity::Info);
        assert_eq!(body.push, false);
    }

    #[test]
    fn falls_back_when_payload_message_missing() {
        let tile = tile_fixture();
        let e1 = ev(1, "agent.notified", 1000, json!({ "kind": "notification" }));
        let refs = vec![&e1];
        let body = AgentNotified
            .evaluate(&tile, &refs, 2000)
            .expect("expected Some(body)");
        assert_eq!(body.message, "claude-code wants attention");
    }

    #[test]
    fn severity_hint_is_respected() {
        let tile = tile_fixture();
        let e1 = ev(1, "agent.notified", 1000,
            json!({ "kind": "notification", "message": "blah", "severity_hint": "urgent" }));
        let refs = vec![&e1];
        let body = AgentNotified.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.severity, Severity::Urgent);
        assert_eq!(body.push, true);
    }

    #[test]
    fn missing_severity_hint_defaults_to_info() {
        let tile = tile_fixture();
        let e1 = ev(1, "agent.notified", 1000, json!({ "kind": "notification", "message": "x" }));
        let refs = vec![&e1];
        let body = AgentNotified.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.severity, Severity::Info);
        assert_eq!(body.push, false);
    }

    #[test]
    fn push_hint_overrides_default() {
        let tile = tile_fixture();
        let e1 = ev(1, "agent.notified", 1000,
            json!({ "kind": "notification", "message": "shh", "severity_hint": "warn", "push_hint": false }));
        let refs = vec![&e1];
        let body = AgentNotified.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.severity, Severity::Warn);
        assert_eq!(body.push, false);
    }

    #[test]
    fn invalid_severity_hint_falls_back_to_info() {
        let tile = tile_fixture();
        let e1 = ev(1, "agent.notified", 1000,
            json!({ "kind": "notification", "severity_hint": "panic" }));
        let refs = vec![&e1];
        let body = AgentNotified.evaluate(&tile, &refs, 2000).expect("expected Some");
        assert_eq!(body.severity, Severity::Info);
    }

    #[test]
    fn does_not_fire_when_latest_event_is_not_agent_notified() {
        let tile = tile_fixture();
        let e1 = ev(
            1,
            "agent.notified",
            1000,
            json!({ "kind": "notification", "message": "hi" }),
        );
        let e2 = ev(2, "turn.prompt_submitted", 2000, json!({}));
        let refs = vec![&e1, &e2];
        assert_eq!(AgentNotified.evaluate(&tile, &refs, 3000), None);
    }
}
