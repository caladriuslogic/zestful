//! Rule trait + small shared types used by the notifications projection.
//!
//! A Rule evaluates a single tile's event stream and either returns
//! Some(NotificationBody) indicating "this rule fires on this tile right
//! now" or None. The engine in mod.rs iterates (tile, rule) pairs and
//! assembles full Notification rows from each Some.

use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Urgent,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Urgent => "urgent",
        };
        f.write_str(s)
    }
}

/// Per-firing fields a Rule produces. The engine fills in the
/// identity-derived fields (id, rule_id, tile_id) and the tile-copied
/// fields (agent, project_label, focus_uri, severity).
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationBody {
    pub message: String,
    pub trigger_event_id: String,
    pub triggered_at_ms: i64,
}

pub trait Rule: Send + Sync {
    /// Stable identifier for this rule, e.g. "agent_completed".
    fn id(&self) -> &'static str;

    /// Per-rule constant severity on day one. Future-compat: a rule
    /// could override per-firing by moving severity into NotificationBody.
    fn severity(&self) -> Severity;

    /// Evaluate this rule for a tile. `tile_events` is the per-tile
    /// event stream, ascending by received_at, filtered to events whose
    /// derived identity tuple matches this tile. `now_ms` is the current
    /// wall-clock unix-ms timestamp, supplied by the engine.
    fn evaluate(
        &self,
        tile: &Tile,
        tile_events: &[&EventRow],
        now_ms: i64,
    ) -> Option<NotificationBody>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_display_is_lowercase() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warn.to_string(), "warn");
        assert_eq!(Severity::Urgent.to_string(), "urgent");
    }

    #[test]
    fn severity_serializes_lowercase() {
        let s = serde_json::to_string(&Severity::Warn).unwrap();
        assert_eq!(s, "\"warn\"");
    }
}
