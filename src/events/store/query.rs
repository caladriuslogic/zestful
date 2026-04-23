//! Read path for the local event store. Shared by the HTTP GET /events
//! handler and the `zestful events` CLI subcommands.

use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Default)]
pub struct ListFilters {
    pub since: Option<i64>,          // received_at lower bound (inclusive, unix ms)
    pub until: Option<i64>,          // received_at upper bound (inclusive, unix ms)
    pub source: Option<String>,
    pub event_type: Option<String>,  // SQL LIKE pattern allowed (use % wildcards)
    pub session_id: Option<String>,
    pub agent: Option<String>,       // extracted from context JSON
}

#[derive(Debug, Clone, Copy)]
pub struct Cursor {
    pub received_at: i64,
    pub id: i64,
}

impl Cursor {
    /// Format as `"<received_at>-<id>"` for wire serialization.
    pub fn to_string(&self) -> String {
        format!("{}-{}", self.received_at, self.id)
    }

    /// Parse from `"<received_at>-<id>"`. Returns None on malformed input.
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once('-')?;
        Some(Self {
            received_at: a.parse().ok()?,
            id: b.parse().ok()?,
        })
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub struct EventRow {
    pub id: i64,
    pub received_at: i64,
    pub event_id: String,
    pub event_type: String,
    pub source: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub host: String,
    pub os_user: String,
    pub device_id: String,
    pub event_ts: i64,
    pub seq: i64,
    pub source_pid: i64,
    pub schema_version: i64,
    pub correlation: Option<serde_json::Value>,
    pub context: Option<serde_json::Value>,
    pub payload: Option<serde_json::Value>,
}

pub fn list(
    conn: &Connection,
    filters: &ListFilters,
    limit: usize,
    cursor: Option<Cursor>,
) -> rusqlite::Result<(Vec<EventRow>, Option<Cursor>)> {
    let mut sql = String::from(
        "SELECT id, received_at, event_id, event_type, source, session_id, project,
                host, os_user, device_id, event_ts, seq, source_pid, schema_version,
                correlation, context, payload
         FROM events WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(since) = filters.since {
        sql.push_str(" AND received_at >= ?");
        params.push(Box::new(since));
    }
    if let Some(until) = filters.until {
        sql.push_str(" AND received_at <= ?");
        params.push(Box::new(until));
    }
    if let Some(s) = &filters.source {
        sql.push_str(" AND source = ?");
        params.push(Box::new(s.clone()));
    }
    if let Some(t) = &filters.event_type {
        sql.push_str(" AND event_type LIKE ?");
        params.push(Box::new(t.clone()));
    }
    if let Some(s) = &filters.session_id {
        sql.push_str(" AND session_id = ?");
        params.push(Box::new(s.clone()));
    }
    if let Some(a) = &filters.agent {
        sql.push_str(" AND json_extract(context, '$.agent') = ?");
        params.push(Box::new(a.clone()));
    }
    if let Some(c) = cursor {
        sql.push_str(" AND (received_at < ? OR (received_at = ? AND id < ?))");
        params.push(Box::new(c.received_at));
        params.push(Box::new(c.received_at));
        params.push(Box::new(c.id));
    }

    // Fetch limit+1 so we can tell if there's more.
    sql.push_str(" ORDER BY received_at DESC, id DESC LIMIT ?");
    let fetch = (limit + 1) as i64;
    params.push(Box::new(fetch));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows_iter = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(EventRow {
            id: row.get(0)?,
            received_at: row.get(1)?,
            event_id: row.get(2)?,
            event_type: row.get(3)?,
            source: row.get(4)?,
            session_id: row.get(5)?,
            project: row.get(6)?,
            host: row.get(7)?,
            os_user: row.get(8)?,
            device_id: row.get(9)?,
            event_ts: row.get(10)?,
            seq: row.get(11)?,
            source_pid: row.get(12)?,
            schema_version: row.get(13)?,
            correlation: row.get::<_, Option<String>>(14)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            context: row.get::<_, Option<String>>(15)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            payload: row.get::<_, Option<String>>(16)?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;

    let mut rows: Vec<EventRow> = Vec::with_capacity(limit + 1);
    for r in rows_iter {
        rows.push(r?);
    }

    let next_cursor = if rows.len() > limit {
        let _extra = rows.pop().unwrap();  // trim the lookahead row
        let last = rows.last().unwrap();
        Some(Cursor { received_at: last.received_at, id: last.id })
    } else {
        None
    };

    Ok((rows, next_cursor))
}

pub fn count(conn: &Connection, filters: &ListFilters) -> rusqlite::Result<i64> {
    let mut sql = String::from("SELECT COUNT(*) FROM events WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(since) = filters.since {
        sql.push_str(" AND received_at >= ?");
        params.push(Box::new(since));
    }
    if let Some(until) = filters.until {
        sql.push_str(" AND received_at <= ?");
        params.push(Box::new(until));
    }
    if let Some(s) = &filters.source {
        sql.push_str(" AND source = ?");
        params.push(Box::new(s.clone()));
    }
    if let Some(t) = &filters.event_type {
        sql.push_str(" AND event_type LIKE ?");
        params.push(Box::new(t.clone()));
    }
    if let Some(s) = &filters.session_id {
        sql.push_str(" AND session_id = ?");
        params.push(Box::new(s.clone()));
    }
    if let Some(a) = &filters.agent {
        sql.push_str(" AND json_extract(context, '$.agent') = ?");
        params.push(Box::new(a.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::{schema::run_migrations, write::insert};
    use rusqlite::Connection;
    use serde_json::json;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    fn fixture(id: &str, event_type: &str, source: &str, session: Option<&str>, agent: &str) -> serde_json::Value {
        let mut corr = serde_json::Map::new();
        if let Some(s) = session {
            corr.insert("session_id".into(), json!(s));
        }
        json!({
            "id": id,
            "schema": 1,
            "ts": 1_234_567_890_000i64,
            "seq": 0,
            "host": "h",
            "os_user": "u",
            "device_id": "d",
            "source": source,
            "source_pid": 1,
            "type": event_type,
            "correlation": corr,
            "context": { "agent": agent }
        })
    }

    #[test]
    fn list_empty_returns_empty() {
        let conn = setup();
        let (rows, next) = list(&conn, &ListFilters::default(), 50, None).unwrap();
        assert!(rows.is_empty());
        assert!(next.is_none());
    }

    #[test]
    fn list_filters_by_source() {
        let conn = setup();
        insert(&conn, &fixture("01", "turn.completed", "claude-code", None, "claude-code")).unwrap();
        insert(&conn, &fixture("02", "turn.completed", "vscode-extension", None, "vscode")).unwrap();
        insert(&conn, &fixture("03", "turn.completed", "claude-code", None, "claude-code")).unwrap();
        let filters = ListFilters { source: Some("claude-code".into()), ..Default::default() };
        let (rows, _) = list(&conn, &filters, 50, None).unwrap();
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.source, "claude-code");
        }
    }

    #[test]
    fn list_filters_by_type_with_like_wildcard() {
        let conn = setup();
        insert(&conn, &fixture("01", "turn.completed", "claude-code", None, "claude-code")).unwrap();
        insert(&conn, &fixture("02", "turn.prompt_submitted", "claude-code", None, "claude-code")).unwrap();
        insert(&conn, &fixture("03", "tool.invoked", "claude-code", None, "claude-code")).unwrap();
        let filters = ListFilters { event_type: Some("turn.%".into()), ..Default::default() };
        let (rows, _) = list(&conn, &filters, 50, None).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn list_filters_by_session() {
        let conn = setup();
        insert(&conn, &fixture("01", "turn.completed", "claude-code", Some("sess-A"), "claude-code")).unwrap();
        insert(&conn, &fixture("02", "turn.completed", "claude-code", Some("sess-B"), "claude-code")).unwrap();
        insert(&conn, &fixture("03", "turn.completed", "claude-code", Some("sess-A"), "claude-code")).unwrap();
        let filters = ListFilters { session_id: Some("sess-A".into()), ..Default::default() };
        let (rows, _) = list(&conn, &filters, 50, None).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn list_filters_by_agent() {
        let conn = setup();
        insert(&conn, &fixture("01", "turn.completed", "claude-code", None, "claude-code")).unwrap();
        insert(&conn, &fixture("02", "editor.window.focused", "vscode-extension", None, "Code")).unwrap();
        let filters = ListFilters { agent: Some("Code".into()), ..Default::default() };
        let (rows, _) = list(&conn, &filters, 50, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "02");
    }

    #[test]
    fn list_pagination_cursor_roundtrip() {
        let conn = setup();
        for i in 0..150 {
            insert(
                &conn,
                &fixture(&format!("{:03}", i), "turn.completed", "claude-code", None, "claude-code"),
            ).unwrap();
        }
        let mut seen = std::collections::HashSet::new();
        let mut cursor: Option<Cursor> = None;
        let mut pages = 0;
        loop {
            let (rows, next) = list(&conn, &ListFilters::default(), 50, cursor).unwrap();
            pages += 1;
            for r in &rows {
                assert!(seen.insert(r.event_id.clone()), "duplicate id {}", r.event_id);
            }
            if next.is_none() {
                break;
            }
            cursor = next;
            if pages > 10 {
                panic!("too many pages — pagination bug");
            }
        }
        assert_eq!(seen.len(), 150);
        assert!(pages >= 3);
    }

    #[test]
    fn count_with_filters() {
        let conn = setup();
        insert(&conn, &fixture("01", "turn.completed", "claude-code", None, "claude-code")).unwrap();
        insert(&conn, &fixture("02", "turn.completed", "vscode-extension", None, "vscode")).unwrap();
        insert(&conn, &fixture("03", "tool.invoked", "claude-code", None, "claude-code")).unwrap();
        assert_eq!(count(&conn, &ListFilters::default()).unwrap(), 3);
        let f = ListFilters { source: Some("claude-code".into()), ..Default::default() };
        assert_eq!(count(&conn, &f).unwrap(), 2);
    }

    #[test]
    fn cursor_format_roundtrip() {
        let c = Cursor { received_at: 1_234_567_890_000, id: 42 };
        let s = c.to_string();
        let back = Cursor::parse(&s).unwrap();
        assert_eq!(back.received_at, c.received_at);
        assert_eq!(back.id, c.id);
    }
}
