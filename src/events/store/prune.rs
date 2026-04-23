//! Size-cap enforcement for the local event store. Triggered every N
//! inserts by the daemon (see store::mod::PRUNE_CHECK_EVERY).

use rusqlite::Connection;

/// Check DB size; if over `max_bytes`, delete oldest rows to get back
/// under 90% of cap, then run VACUUM to reclaim disk. No-op when
/// `max_bytes == 0` (unbounded) or when the store is already under cap.
///
/// VACUUM (rather than incremental_vacuum) is used because the current
/// init() PRAGMA order locks auto_vacuum=NONE on a fresh WAL database.
/// See the inline note at the VACUUM call for details.
pub fn check_and_enforce(conn: &Connection, max_bytes: u64) -> rusqlite::Result<PruneOutcome> {
    if max_bytes == 0 {
        return Ok(PruneOutcome::Skipped);
    }
    let size = db_size_bytes(conn)?;
    if size <= max_bytes {
        return Ok(PruneOutcome::Skipped);
    }

    let total_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
    if total_rows == 0 {
        // Size is over cap but the events table is empty — overhead only.
        return Ok(PruneOutcome::Skipped);
    }
    // Target keeping only (max_bytes * 90% / current_size) fraction of rows.
    // Using a proportional approach avoids the schema-overhead attribution
    // problem: dividing db_size by row_count includes index/schema pages that
    // don't shrink when rows are deleted, causing naïve avg-byte formulas to
    // under-delete. Here we aim to shrink to 90% of cap (10% safety margin).
    let target_bytes = max_bytes * 9 / 10;
    let rows_to_keep = ((total_rows as u64) * target_bytes / size) as i64;
    let rows_to_drop = (total_rows - rows_to_keep).max(1);

    let deleted = conn.execute(
        "DELETE FROM events WHERE id IN (
            SELECT id FROM events ORDER BY received_at ASC LIMIT ?
         )",
        [rows_to_drop],
    )? as i64;

    // Reclaim space. Full VACUUM works correctly in WAL mode regardless of
    // auto_vacuum setting. incremental_vacuum is a no-op in WAL mode unless
    // auto_vacuum was set BEFORE journal_mode=WAL on a fresh database.
    conn.execute_batch("VACUUM")?;

    let final_bytes = db_size_bytes(conn)?;
    Ok(PruneOutcome::Pruned {
        rows_deleted: deleted,
        final_bytes,
    })
}

#[derive(Debug, PartialEq)]
pub enum PruneOutcome {
    Skipped,              // under cap or unbounded
    Pruned { rows_deleted: i64, final_bytes: u64 },
}

fn db_size_bytes(conn: &Connection) -> rusqlite::Result<u64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    Ok((page_count as u64) * (page_size as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::{schema::run_migrations, write::insert};
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::NamedTempFile;

    fn open_on_disk() -> (NamedTempFile, Connection) {
        let f = NamedTempFile::new().unwrap();
        let conn = Connection::open(f.path()).unwrap();
        // Intentionally mirrors production init()'s PRAGMA order (journal_mode
        // first). The auto_vacuum=INCREMENTAL is silently ignored by SQLite
        // because WAL already wrote pages — see prune.rs's VACUUM note.
        // Production init() should be reordered as a follow-up; when that
        // happens, swap these two lines too.
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL").unwrap();
        run_migrations(&conn).unwrap();
        (f, conn)
    }

    fn fixture(id: &str, pad_kb: usize) -> serde_json::Value {
        json!({
            "id": id,
            "schema": 1,
            "ts": 1_234_567_890_000i64,
            "seq": 0,
            "host": "h",
            "os_user": "u",
            "device_id": "d",
            "source": "claude-code",
            "source_pid": 1,
            "type": "turn.completed",
            "payload": { "pad": "x".repeat(pad_kb * 1024) }
        })
    }

    #[test]
    fn prune_no_op_when_unbounded() {
        let (_f, conn) = open_on_disk();
        for i in 0..10 {
            insert(&conn, &fixture(&format!("{:03}", i), 1)).unwrap();
        }
        let out = check_and_enforce(&conn, 0).unwrap();
        assert_eq!(out, PruneOutcome::Skipped);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 10);
    }

    #[test]
    fn prune_no_op_when_under_cap() {
        let (_f, conn) = open_on_disk();
        for i in 0..5 {
            insert(&conn, &fixture(&format!("{:03}", i), 1)).unwrap();
        }
        // 10 MB cap, well above 5 × 1KB rows.
        let out = check_and_enforce(&conn, 10 * 1024 * 1024).unwrap();
        assert_eq!(out, PruneOutcome::Skipped);
    }

    #[test]
    fn prune_enforces_size_cap_and_evicts_oldest() {
        let (_f, conn) = open_on_disk();
        // Insert 30 rows × ~10 KB payload each ≈ 300 KB of data (plus SQLite overhead).
        for i in 0..30 {
            insert(&conn, &fixture(&format!("{:03}", i), 10)).unwrap();
        }
        // Cap at 175 KB — should prune roughly half the rows. The original
        // 150 KB target was below the ~163 KB vacuum floor for the events
        // schema (16 columns + 4 indexes), so 150 KB is physically unreachable.
        let cap = 175 * 1024;
        let out = check_and_enforce(&conn, cap).unwrap();
        match out {
            PruneOutcome::Pruned { rows_deleted, final_bytes } => {
                assert!(rows_deleted > 0, "expected some rows deleted");
                assert!(
                    final_bytes <= cap,
                    "final_bytes ({}) should be <= cap ({}) after prune",
                    final_bytes, cap
                );
            }
            other => panic!("expected Pruned, got {:?}", other),
        }

        // Oldest IDs should be gone. Highest surviving id should be 30;
        // lowest surviving id should be > 1.
        let (min_id, max_id): (i64, i64) = conn
            .query_row("SELECT MIN(id), MAX(id) FROM events", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert!(min_id > 1, "oldest row should have been evicted, min_id = {}", min_id);
        assert_eq!(max_id, 30);
    }

    #[test]
    fn prune_deletes_solo_row_when_over_cap() {
        // When there's exactly one row and it's over cap, the .max(1) clamp
        // ensures we delete it rather than silently doing nothing. Without the
        // clamp, integer division could compute rows_to_drop = 0.
        let (_f, conn) = open_on_disk();
        insert(&conn, &fixture("only", 500)).unwrap();  // 500 KB single row
        let cap = 200 * 1024;  // 200 KB cap, well below the row size
        let out = check_and_enforce(&conn, cap).unwrap();
        match out {
            PruneOutcome::Pruned { rows_deleted, .. } => {
                assert_eq!(rows_deleted, 1);
            }
            other => panic!("expected Pruned, got {:?}", other),
        }
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
