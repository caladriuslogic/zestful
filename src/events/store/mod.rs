//! Local event store backed by SQLite. Every envelope accepted by the
//! daemon is persisted here; HTTP GET /events and the `zestful events`
//! CLI read through the `query` submodule.

pub mod schema;
pub mod write;
pub mod query;
pub mod prune;

use rusqlite::Connection;
use std::path::Path;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};

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
///
/// Visibility: widened from `pub(crate)` to `pub` so cross-module
/// integration tests (e.g. `scraper::tests`) can verify db state directly
/// after running the dispatch loop end-to-end.
pub fn conn() -> &'static Mutex<Connection> {
    CONNECTION.get().expect("events::store::init() must be called first")
}

/// Test-only init: open an in-memory SQLite, run migrations, and store
/// it in the `CONNECTION` OnceLock. Safe to call from many tests in the
/// same process — `Once::call_once` ensures we attempt the init at most
/// once.
///
/// If another test (e.g. `cmd::daemon::tests::app`) has already populated
/// `CONNECTION` via real `init()`, the `CONNECTION.set` call here returns
/// Err and the in-memory connection is silently dropped. That's
/// intentional: any pre-existing CONNECTION already has migrations
/// applied (because `init()` runs them) and the same `events` schema, so
/// integration tests that just need a working store can proceed.
#[cfg(test)]
pub fn init_for_tests() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let conn = Connection::open_in_memory().expect("open_in_memory");
        // Apply same PRAGMAs as init() (in-memory ignores some, that's fine).
        let _ = conn.pragma_update(None, "foreign_keys", "ON");
        schema::run_migrations(&conn).expect("migrations should succeed in tests");
        let _ = CONNECTION.set(Mutex::new(conn));
    });
}

static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);
static PRUNE_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that an insert happened. Every PRUNE_CHECK_EVERY inserts,
/// spawn a background task that checks the DB size and prunes if over
/// cap.
///
/// Non-blocking for the triggering insert. A PRUNE_IN_FLIGHT guard
/// ensures at most one prune task is active at a time — if a prune is
/// already running when the next trigger fires, this call is a no-op.
///
/// While a prune is running it holds the connection mutex for the
/// duration of the VACUUM (seconds, for large stores). Other inserts
/// and queries block on the mutex during that window. This is
/// acceptable at the current traffic profile because pruning runs
/// infrequently (once per 100 accepted events).
pub fn record_insert_and_maybe_prune(max_bytes: u64) {
    let n = WRITE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    if n % PRUNE_CHECK_EVERY != 0 {
        return;
    }
    // Skip if a prune is already in flight.
    if PRUNE_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let c = conn().lock().unwrap();
            prune::check_and_enforce(&c, max_bytes)
        })
        .await
        .expect("prune task panicked");
        match result {
            Ok(prune::PruneOutcome::Pruned { rows_deleted, final_bytes }) => {
                crate::log::log(
                    "events",
                    &format!(
                        "pruned {} rows, db size now {} KB",
                        rows_deleted,
                        final_bytes / 1024
                    ),
                );
            }
            Ok(prune::PruneOutcome::Skipped) => {}
            Err(e) => {
                crate::log::log("events", &format!("prune error: {}", e));
            }
        }
        PRUNE_IN_FLIGHT.store(false, Ordering::Release);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Test migration and PRAGMA setup by opening a private connection
    /// (not via the global OnceLock). This avoids a process-wide init()
    /// conflict when the daemon test module also initializes the store.
    #[test]
    fn init_opens_and_migrates() {
        let f = NamedTempFile::new().unwrap();
        // Open and configure directly, mirroring what init() does internally.
        let conn = Connection::open(f.path()).expect("open");
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "synchronous", "NORMAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.pragma_update(None, "busy_timeout", 5000).unwrap();
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL").unwrap();
        schema::run_migrations(&conn).expect("migrations should succeed on empty file");

        assert_eq!(schema::current_version(&conn).unwrap(), 2);

        // PRAGMAs should have landed — catch silent WAL downgrades.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
