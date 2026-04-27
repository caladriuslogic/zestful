//! Migration runner for the local event store. Reads numbered SQL files
//! embedded via include_str! and applies them in order, tracking applied
//! versions in _schema_migrations.

use rusqlite::Connection;

const MIGRATION_001: &str = include_str!("migrations/001_initial.sql");

pub fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // Ensure the tracking table exists first. The migration file itself
    // includes a CREATE TABLE IF NOT EXISTS for _schema_migrations, but
    // we need to read from it *before* deciding whether to run migration
    // 001, so bootstrap it here.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
        [],
    )?;

    if !is_applied(conn, 1)? {
        conn.execute_batch(MIGRATION_001)?;
        record_applied(conn, 1)?;
    }
    Ok(())
}

/// Returns the highest applied migration version, or 0 if none applied.
#[allow(dead_code)]
pub fn current_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM _schema_migrations",
        [],
        |row| row.get(0),
    )
}

fn is_applied(conn: &Connection, version: i64) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM _schema_migrations WHERE version = ?",
        [version],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn record_applied(conn: &Connection, version: i64) -> rusqlite::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO _schema_migrations (version, applied_at) VALUES (?, ?)",
        [version, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_memory() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn migrations_run_clean_on_empty_db() {
        let conn = open_memory();
        run_migrations(&conn).expect("migrations should run clean");
        assert_eq!(current_version(&conn).unwrap(), 1);

        // Verify the events table exists and has the expected columns.
        let mut stmt = conn.prepare("PRAGMA table_info(events)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for expected in [
            "id", "received_at", "event_id", "schema_version", "event_ts",
            "seq", "host", "os_user", "device_id", "source", "source_pid",
            "event_type", "session_id", "project", "correlation",
            "context", "payload",
        ] {
            assert!(cols.iter().any(|c| c == expected),
                    "events table missing column {}", expected);
        }

        // Verify all 4 indexes were created.
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='events' AND sql IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 4, "expected 4 indexes on events table");
    }

    #[test]
    fn migrations_idempotent() {
        let conn = open_memory();
        run_migrations(&conn).unwrap();
        let v1 = current_version(&conn).unwrap();
        run_migrations(&conn).unwrap();
        let v2 = current_version(&conn).unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1, 1);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM _schema_migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn migrations_idempotent_across_reopen() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new().unwrap();

        // First open + migrate.
        {
            let conn = Connection::open(f.path()).unwrap();
            run_migrations(&conn).unwrap();
            assert_eq!(current_version(&conn).unwrap(), 1);
        }

        // Reopen; should be a no-op.
        {
            let conn = Connection::open(f.path()).unwrap();
            run_migrations(&conn).unwrap();
            assert_eq!(current_version(&conn).unwrap(), 1);
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM _schema_migrations", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
        }
    }
}
