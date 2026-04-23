//! Local event store backed by SQLite. Every envelope accepted by the
//! daemon is persisted here; HTTP GET /events and the `zestful events`
//! CLI read through the `query` submodule.

pub mod schema;
pub mod write;
pub mod query;
// Submodules added in later tasks: prune.

use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Hardcoded cap; 0 = unbounded. Change in code if tuning.
pub const DEFAULT_MAX_BYTES: u64 = 1_073_741_824;

/// Prune check runs every N inserts (tune in code if needed).
pub const PRUNE_CHECK_EVERY: u64 = 100;

/// Process-global connection, set by `init()` on daemon startup.
static CONNECTION: OnceLock<Mutex<Connection>> = OnceLock::new();

/// Open the store at `path`, apply migrations, set PRAGMAs.
///
/// Call once on daemon startup. Calling this more than once per process
/// PANICS — on a single-process daemon, double-init is a programmer
/// error, not a recoverable condition.
///
/// A migration failure is fatal — caller should log and exit.
pub fn init(path: &Path) -> rusqlite::Result<()> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    schema::run_migrations(&conn)?;
    if CONNECTION.set(Mutex::new(conn)).is_err() {
        panic!("events::store::init() called more than once");
    }
    Ok(())
}

/// Acquire the process-global connection. Panics if `init` wasn't called.
/// Internal use only — callers should go through write/query/prune.
pub(crate) fn conn() -> &'static Mutex<Connection> {
    CONNECTION.get().expect("events::store::init() must be called first")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn init_opens_and_migrates() {
        let f = NamedTempFile::new().unwrap();
        init(f.path()).expect("init should succeed on empty file");

        let conn = conn().lock().unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM _schema_migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);

        // PRAGMAs should have landed — catch silent WAL downgrades.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
