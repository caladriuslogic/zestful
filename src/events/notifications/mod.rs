//! Notifications projection — derives active notifications from the
//! events table by running a small set of Rule implementations over
//! per-tile event streams. See spec
//! 2026-04-24-notifications-projection-design.md.

pub mod notification;
pub mod rule;
pub mod rules;

use crate::events::store::query::EventRow;
use crate::events::tiles::tile::Tile;
use notification::Notification;
use rusqlite::Connection;
use std::collections::HashMap;

/// Compute the notifications projection over events with
/// received_at >= since_ms. Pure function over the events table —
/// no caching, no incremental state.
///
/// Returns notifications sorted by triggered_at_ms DESC.
pub fn compute(_conn: &Connection, _since_ms: i64) -> rusqlite::Result<Vec<Notification>> {
    // Stub — real implementation lands in Task 5 after rules are registered.
    Ok(Vec::new())
}

/// Bucket events into per-tile streams, keyed by tile id. Events that
/// don't derive a tile identity (no context, malformed, etc.) are
/// dropped. Implementation lands in Task 5.
#[allow(dead_code)]
fn bucket_events_by_tile<'a>(_events: &'a [EventRow]) -> HashMap<String, Vec<&'a EventRow>> {
    HashMap::new()
}

/// Assemble a full Notification row from a Rule's output and the tile
/// it fired on. Used by compute() once the real implementation lands.
#[allow(dead_code)]
fn assemble(_tile: &Tile, _rule: &dyn rule::Rule, _body: rule::NotificationBody) -> Notification {
    unreachable!("filled in in Task 5")
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

    #[test]
    fn compute_with_empty_events_returns_empty() {
        let (_f, conn) = open_test_conn();
        let out = compute(&conn, 0).unwrap();
        assert!(out.is_empty());
    }
}
