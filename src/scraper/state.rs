//! SQLite-backed per-file state for the scraper. Wraps the
//! `scraper_file_state` table from migration 002.

use rusqlite::{params, Connection};

/// Per-file state. `path` is the canonical absolute path of the
/// transcript file on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct FileState {
    pub path: String,
    pub agent: String,
    pub fingerprint: String,
    pub last_offset: u64,
    pub last_emit_ts: i64,
}

impl FileState {
    /// State for a never-seen-before file. Caller fills in `path` and
    /// `agent`; everything else is the "no progress yet" defaults.
    pub fn fresh(path: String, agent: String) -> Self {
        Self {
            path,
            agent,
            fingerprint: String::new(),
            last_offset: 0,
            last_emit_ts: 0,
        }
    }
}

/// Read the row for `path`, or return a fresh `FileState` if no row exists.
pub fn get_or_fresh(
    conn: &Connection,
    path: &str,
    agent: &str,
) -> rusqlite::Result<FileState> {
    let mut stmt = conn.prepare(
        "SELECT agent, fingerprint, last_offset, last_emit_ts \
         FROM scraper_file_state WHERE path = ?",
    )?;
    let mut rows = stmt.query(params![path])?;
    if let Some(row) = rows.next()? {
        Ok(FileState {
            path: path.to_string(),
            agent: row.get(0)?,
            fingerprint: row.get(1)?,
            last_offset: row.get::<_, i64>(2)? as u64,
            last_emit_ts: row.get(3)?,
        })
    } else {
        Ok(FileState::fresh(path.to_string(), agent.to_string()))
    }
}

/// Upsert the row by `path`.
pub fn upsert(conn: &Connection, s: &FileState) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO scraper_file_state \
            (path, agent, fingerprint, last_offset, last_emit_ts) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(path) DO UPDATE SET \
            agent = excluded.agent, \
            fingerprint = excluded.fingerprint, \
            last_offset = excluded.last_offset, \
            last_emit_ts = excluded.last_emit_ts",
        params![
            s.path,
            s.agent,
            s.fingerprint,
            s.last_offset as i64,
            s.last_emit_ts,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::schema::run_migrations;

    fn open() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        run_migrations(&c).unwrap();
        c
    }

    #[test]
    fn fresh_state_when_no_row() {
        let c = open();
        let s = get_or_fresh(&c, "/tmp/x.jsonl", "claude-code").unwrap();
        assert_eq!(s.path, "/tmp/x.jsonl");
        assert_eq!(s.agent, "claude-code");
        assert_eq!(s.last_offset, 0);
        assert_eq!(s.last_emit_ts, 0);
        assert_eq!(s.fingerprint, "");
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let c = open();
        let s = FileState {
            path: "/tmp/x.jsonl".into(),
            agent: "claude-code".into(),
            fingerprint: "100:1234567890".into(),
            last_offset: 256,
            last_emit_ts: 1700000000000,
        };
        upsert(&c, &s).unwrap();
        let got = get_or_fresh(&c, "/tmp/x.jsonl", "claude-code").unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn upsert_overwrites() {
        let c = open();
        let mut s = FileState {
            path: "/tmp/x.jsonl".into(),
            agent: "claude-code".into(),
            fingerprint: "100:1".into(),
            last_offset: 256,
            last_emit_ts: 1,
        };
        upsert(&c, &s).unwrap();
        s.last_offset = 512;
        s.fingerprint = "200:2".into();
        s.last_emit_ts = 2;
        upsert(&c, &s).unwrap();
        let got = get_or_fresh(&c, "/tmp/x.jsonl", "claude-code").unwrap();
        assert_eq!(got.last_offset, 512);
        assert_eq!(got.fingerprint, "200:2");
        assert_eq!(got.last_emit_ts, 2);
    }

}
