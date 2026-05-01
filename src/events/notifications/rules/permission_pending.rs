//! Rule B: permission_pending — fires when a permission.requested has
//! been unresolved for >10s on a tile.

use crate::events::notifications::rule::{NotificationBody, Rule, Severity};
use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;

pub struct PermissionPending;

const THRESHOLD_MS: i64 = 10_000;

impl Rule for PermissionPending {
    fn id(&self) -> &'static str {
        "permission_pending"
    }
    fn evaluate(
        &self,
        tile: &Tile,
        tile_events: &[&EventRow],
        now_ms: i64,
    ) -> Option<NotificationBody> {
        // Walk events newest-to-oldest, tracking whether we've seen a
        // resolving event after the latest permission.requested.
        let mut seen_resolution = false;
        for e in tile_events.iter().rev() {
            match e.event_type.as_str() {
                "permission.granted" | "permission.denied" => {
                    seen_resolution = true;
                }
                "permission.requested" => {
                    if seen_resolution {
                        return None; // the latest permission has already been resolved
                    }
                    if now_ms - e.received_at <= THRESHOLD_MS {
                        return None;
                    }
                    let tool = e
                        .payload
                        .as_ref()
                        .and_then(|p| p.get("tool_name"))
                        .and_then(|v| v.as_str());
                    let message = build_message(&tile.agent, tool, tile.project_label.as_deref());
                    return Some(NotificationBody {
                        message,
                        trigger_event_id: e.event_id.clone(),
                        triggered_at_ms: e.event_ts,
                        severity: Severity::Urgent,
                        push: true,
                    });
                }
                _ => {}
            }
        }
        None
    }
}

fn build_message(agent: &str, tool: Option<&str>, project: Option<&str>) -> String {
    let mut s = format!("{} waiting on permission", agent);
    if let Some(t) = tool {
        s.push_str(" for ");
        s.push_str(t);
    }
    if let Some(p) = project {
        s.push_str(" on ");
        s.push_str(p);
    }
    s
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

    fn ev_perm_req(id: i64, received_at: i64, tool: Option<&str>) -> EventRow {
        let payload = match tool {
            Some(t) => json!({ "kind": "tool", "message": "need permission", "tool_name": t }),
            None => json!({ "kind": "other", "message": "need permission" }),
        };
        EventRow {
            id,
            received_at,
            event_id: format!("evt-{}", id),
            event_type: "permission.requested".to_string(),
            source: "claude-code".to_string(),
            session_id: None,
            project: None,
            host: "h".to_string(),
            os_user: "u".to_string(),
            device_id: "d".to_string(),
            event_ts: received_at,
            seq: 0,
            source_pid: 1,
            schema_version: 1,
            correlation: None,
            context: Some(json!({})),
            payload: Some(payload),
        }
    }

    fn ev_granted(id: i64, received_at: i64) -> EventRow {
        EventRow {
            id,
            received_at,
            event_id: format!("evt-{}", id),
            event_type: "permission.granted".to_string(),
            source: "claude-code".to_string(),
            session_id: None,
            project: None,
            host: "h".to_string(),
            os_user: "u".to_string(),
            device_id: "d".to_string(),
            event_ts: received_at,
            seq: 0,
            source_pid: 1,
            schema_version: 1,
            correlation: None,
            context: Some(json!({})),
            payload: Some(json!({})),
        }
    }

    #[test]
    fn fires_after_threshold_with_no_resolution() {
        let tile = tile_fixture();
        let req = ev_perm_req(1, 1000, Some("bash"));
        let refs = vec![&req];
        // now_ms = 12000 → delta = 11000 ms > 10_000 threshold
        let body = PermissionPending
            .evaluate(&tile, &refs, 12_000)
            .expect("expected Some(body)");
        assert_eq!(body.trigger_event_id, "evt-1");
        assert_eq!(body.triggered_at_ms, 1000);
        assert_eq!(
            body.message,
            "claude-code waiting on permission for bash on zestful"
        );
        assert_eq!(body.severity, Severity::Urgent);
        assert_eq!(body.push, true);
    }

    #[test]
    fn does_not_fire_before_threshold() {
        let tile = tile_fixture();
        let req = ev_perm_req(1, 1000, None);
        let refs = vec![&req];
        // now_ms = 5000 → delta = 4000 ms < 10_000 threshold
        assert_eq!(PermissionPending.evaluate(&tile, &refs, 5_000), None);
    }

    #[test]
    fn self_resolves_on_permission_granted() {
        let tile = tile_fixture();
        let req = ev_perm_req(1, 1000, Some("bash"));
        let granted = ev_granted(2, 5000);
        let refs = vec![&req, &granted];
        // Even though 12000 > 1000 + 10_000, the grant resolves it.
        assert_eq!(PermissionPending.evaluate(&tile, &refs, 12_000), None);
    }

    #[test]
    fn message_omits_tool_and_project_when_absent() {
        let mut tile = tile_fixture();
        tile.project_label = None;
        let req = ev_perm_req(1, 1000, None);
        let refs = vec![&req];
        let body = PermissionPending
            .evaluate(&tile, &refs, 12_000)
            .expect("expected Some(body)");
        assert_eq!(body.message, "claude-code waiting on permission");
        assert_eq!(body.severity, Severity::Urgent);
        assert_eq!(body.push, true);
    }
}
