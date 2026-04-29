//! Rule trait + small shared types used by the notifications projection.
//!
//! A Rule evaluates a single tile's event stream and either returns
//! Some(NotificationBody) indicating "this rule fires on this tile right
//! now" or None. The engine in mod.rs iterates (tile, rule) pairs and
//! assembles full Notification rows from each Some.

pub use crate::events::severity::Severity;
use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;

/// Per-firing fields a Rule produces. The engine fills in the
/// identity-derived fields (id, rule_id, tile_id) and the tile-copied
/// fields (agent, project_label, focus_uri).
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationBody {
    pub message: String,
    pub trigger_event_id: String,
    pub triggered_at_ms: i64,
    pub severity: Severity,
    pub push: bool,
}

pub trait Rule: Send + Sync {
    /// Stable identifier for this rule, e.g. "agent_completed".
    fn id(&self) -> &'static str;

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
