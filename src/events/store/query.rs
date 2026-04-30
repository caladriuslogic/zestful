//! Read path for the local event store. Shared by the HTTP GET /events
//! handler and the `zestful events` CLI subcommands.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Filters for list / count queries. All fields are optional; each
/// populated field contributes an `AND` clause to the generated SQL.
#[derive(Debug, Clone, Default)]
pub struct ListFilters {
    /// Lower bound on received_at (unix ms, inclusive).
    pub since: Option<i64>,
    /// Upper bound on received_at (unix ms, inclusive).
    pub until: Option<i64>,
    /// Exact match on the emitter's source slug.
    pub source: Option<String>,
    /// Event type filter. If the value contains a `%`, SQL LIKE is used
    /// (where `_` also matches a single character, standard SQL semantics).
    /// Otherwise exact match via `=`. Pass `"turn.%"` to match all turn
    /// events, pass `"turn.completed"` for the exact name.
    pub event_type: Option<String>,
    /// Exact match on correlation.session_id.
    pub session_id: Option<String>,
    /// Exact match on context.agent via json_extract.
    pub agent: Option<String>,
}

/// Opaque pagination cursor. Produced by `list()`, passed back to the
/// next `list()` call to get the next page. Callers should not
/// construct values manually — use `parse()` on a string previously
/// produced by `Display`.
#[derive(Debug, Clone, Copy)]
pub struct Cursor {
    pub received_at: i64,
    pub id: i64,
}

impl Cursor {
    /// Parse from the `"<received_at>-<id>"` wire format produced by
    /// the `Display` impl. Returns None on malformed input.
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once('-')?;
        Some(Self {
            received_at: a.parse().ok()?,
            id: b.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.received_at, self.id)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
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

/// Read events matching `filters`, ordered newest-first
/// (`received_at DESC, id DESC`), paginated by cursor.
///
/// `limit` is the maximum page size; pass `limit >= 1`. A `limit == 0`
/// call returns an empty result with no next cursor.
///
/// Returns `(rows, next_cursor)`. `next_cursor` is `Some(_)` iff there
/// are further pages; pass it back into a subsequent call's `cursor`
/// parameter to get the next page.
pub fn list(
    conn: &Connection,
    filters: &ListFilters,
    limit: usize,
    cursor: Option<Cursor>,
) -> rusqlite::Result<(Vec<EventRow>, Option<Cursor>)> {
    if limit == 0 {
        return Ok((Vec::new(), None));
    }

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
        // Use LIKE only when the caller explicitly opts in with a '%'.
        // Otherwise `_` in a literal event name (e.g. "turn.prompt_submitted")
        // would be treated as a single-char wildcard and silently overmatch.
        if t.contains('%') {
            sql.push_str(" AND event_type LIKE ?");
        } else {
            sql.push_str(" AND event_type = ?");
        }
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
            correlation: parse_json_column(row.get::<_, Option<String>>(14)?, "correlation")?,
            context: parse_json_column(row.get::<_, Option<String>>(15)?, "context")?,
            payload: parse_json_column(row.get::<_, Option<String>>(16)?, "payload")?,
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

/// Count events matching `filters`. Same WHERE clause semantics as
/// `list()`; does not accept a cursor.
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
        // Use LIKE only when the caller explicitly opts in with a '%'.
        // Otherwise `_` in a literal event name (e.g. "turn.prompt_submitted")
        // would be treated as a single-char wildcard and silently overmatch.
        if t.contains('%') {
            sql.push_str(" AND event_type LIKE ?");
        } else {
            sql.push_str(" AND event_type = ?");
        }
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

fn parse_json_column(
    raw: Option<String>,
    column_name: &'static str,
) -> rusqlite::Result<Option<serde_json::Value>> {
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str(&s).map(Some).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("{} JSON parse error: {}", column_name, e).into(),
            )
        }),
    }
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

    #[test]
    fn list_filters_by_since_and_until() {
        let conn = setup();
        // Insert three rows with slightly different received_at by manually
        // forcing them apart. Use a small sleep is flaky; easier: insert then
        // update received_at directly.
        for i in 0..3 {
            insert(
                &conn,
                &fixture(&format!("{:03}", i), "turn.completed", "claude-code", None, "claude-code"),
            ).unwrap();
        }
        // Override received_at deterministically.
        conn.execute("UPDATE events SET received_at = 1000 WHERE event_id = '000'", []).unwrap();
        conn.execute("UPDATE events SET received_at = 2000 WHERE event_id = '001'", []).unwrap();
        conn.execute("UPDATE events SET received_at = 3000 WHERE event_id = '002'", []).unwrap();

        let f = ListFilters { since: Some(2000), ..Default::default() };
        let (rows, _) = list(&conn, &f, 50, None).unwrap();
        assert_eq!(rows.len(), 2);

        let f = ListFilters { until: Some(2000), ..Default::default() };
        let (rows, _) = list(&conn, &f, 50, None).unwrap();
        assert_eq!(rows.len(), 2);

        let f = ListFilters { since: Some(2000), until: Some(2000), ..Default::default() };
        let (rows, _) = list(&conn, &f, 50, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "001");
    }

    #[test]
    fn list_exact_match_on_event_type_does_not_wildcard() {
        let conn = setup();
        // event_type with an underscore should NOT match other events that
        // differ only in that character position (SQL LIKE hazard regression).
        insert(&conn, &fixture("01", "turn.prompt_submitted", "claude-code", None, "claude-code")).unwrap();
        insert(&conn, &fixture("02", "turn.promptXsubmitted", "claude-code", None, "claude-code")).unwrap();

        let f = ListFilters {
            event_type: Some("turn.prompt_submitted".into()),
            ..Default::default()
        };
        let (rows, _) = list(&conn, &f, 50, None).unwrap();
        assert_eq!(rows.len(), 1, "exact match should not wildcard _");
        assert_eq!(rows[0].event_id, "01");
    }

    #[test]
    fn list_limit_zero_returns_empty() {
        let conn = setup();
        insert(&conn, &fixture("01", "turn.completed", "claude-code", None, "claude-code")).unwrap();
        let (rows, next) = list(&conn, &ListFilters::default(), 0, None).unwrap();
        assert!(rows.is_empty());
        assert!(next.is_none());
    }
}
