//! Write path for the local event store. One public function: `insert`.

use rusqlite::Connection;
use serde_json::Value;

#[derive(Debug, PartialEq)]
pub enum InsertOutcome {
    Inserted(i64),      // local rowid
    DuplicateIgnored,   // event_id already present
}

/// Insert one envelope. Returns `Inserted(rowid)` or `DuplicateIgnored`.
/// Uses INSERT OR IGNORE on the UNIQUE(event_id) constraint to make
/// retries idempotent.
pub fn insert(conn: &Connection, envelope: &Value) -> rusqlite::Result<InsertOutcome> {
    let obj = envelope
        .as_object()
        .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure(
            "envelope is not an object".into(),
        ))?;

    let event_id = obj.get("id").and_then(Value::as_str)
        .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure("missing id".into()))?;
    let schema_version = obj.get("schema").and_then(Value::as_i64)
        .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure("missing schema".into()))?;
    let event_ts = obj.get("ts").and_then(Value::as_i64)
        .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure("missing ts".into()))?;
    let seq = obj.get("seq").and_then(Value::as_i64).unwrap_or(0);
    let host = obj.get("host").and_then(Value::as_str).unwrap_or("");
    let os_user = obj.get("os_user").and_then(Value::as_str).unwrap_or("");
    let device_id = obj.get("device_id").and_then(Value::as_str).unwrap_or("");
    let source = obj.get("source").and_then(Value::as_str).unwrap_or("");
    let source_pid = obj.get("source_pid").and_then(Value::as_i64).unwrap_or(0);
    let event_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    let correlation = obj.get("correlation");
    let session_id = correlation
        .and_then(|c| c.get("session_id"))
        .and_then(Value::as_str)
        .map(String::from);
    let project = obj
        .get("context")
        .and_then(|c| c.get("project"))
        .and_then(Value::as_str)
        .map(String::from);

    let correlation_json = correlation.map(|v| v.to_string());
    let context_json = obj.get("context").map(|v| v.to_string());
    let payload_json = obj.get("payload").map(|v| v.to_string());

    let received_at = now_unix_ms();

    let rows_affected = conn.execute(
        "INSERT OR IGNORE INTO events (
            received_at, event_id, schema_version, event_ts, seq,
            host, os_user, device_id, source, source_pid, event_type,
            session_id, project, correlation, context, payload
         ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            received_at,
            event_id,
            schema_version,
            event_ts,
            seq,
            host,
            os_user,
            device_id,
            source,
            source_pid,
            event_type,
            session_id,
            project,
            correlation_json,
            context_json,
            payload_json,
        ],
    )?;

    if rows_affected == 0 {
        Ok(InsertOutcome::DuplicateIgnored)
    } else {
        Ok(InsertOutcome::Inserted(conn.last_insert_rowid()))
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::schema::run_migrations;
    use rusqlite::Connection;
    use serde_json::json;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    fn fixture(id: &str) -> Value {
        json!({
            "id": id,
            "schema": 1,
            "ts": 1_234_567_890_000i64,
            "seq": 0,
            "host": "morrow.local",
            "os_user": "jmorrow",
            "device_id": "01JGYJ12345",
            "source": "claude-code",
            "source_pid": 12345,
            "type": "turn.completed",
            "correlation": { "session_id": "sess-abc" },
            "context": { "agent": "claude-code", "project": "zestful" },
            "payload": { "duration_ms": 2500 }
        })
    }

    #[test]
    fn insert_stores_full_envelope() {
        let conn = setup();
        let env = fixture("01KPVS12345");
        let out = insert(&conn, &env).unwrap();
        assert!(matches!(out, InsertOutcome::Inserted(_)));

        // Verify all promoted columns landed.
        let (event_id, event_type, source, session_id, project): (String, String, String, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT event_id, event_type, source, session_id, project FROM events WHERE event_id = ?",
                ["01KPVS12345"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(event_id, "01KPVS12345");
        assert_eq!(event_type, "turn.completed");
        assert_eq!(source, "claude-code");
        assert_eq!(session_id.as_deref(), Some("sess-abc"));
        assert_eq!(project.as_deref(), Some("zestful"));

        // Verify context JSON round-trips.
        let context: String = conn
            .query_row(
                "SELECT context FROM events WHERE event_id = ?",
                ["01KPVS12345"],
                |row| row.get(0),
            )
            .unwrap();
        let parsed: Value = serde_json::from_str(&context).unwrap();
        assert_eq!(parsed["agent"], "claude-code");
        assert_eq!(parsed["project"], "zestful");
    }

    #[test]
    fn insert_dedupes_by_event_id() {
        let conn = setup();
        let env = fixture("01KPVSDUPE");
        let first = insert(&conn, &env).unwrap();
        let second = insert(&conn, &env).unwrap();
        assert!(matches!(first, InsertOutcome::Inserted(_)));
        assert_eq!(second, InsertOutcome::DuplicateIgnored);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_handles_missing_optional_fields() {
        let conn = setup();
        // Minimal envelope — no correlation, no context, no payload.
        let env = json!({
            "id": "01KPVSMIN01",
            "schema": 1,
            "ts": 1_234_567_890_000i64,
            "seq": 0,
            "host": "h",
            "os_user": "u",
            "device_id": "d",
            "source": "claude-code",
            "source_pid": 1,
            "type": "turn.prompt_submitted"
        });
        let out = insert(&conn, &env).unwrap();
        assert!(matches!(out, InsertOutcome::Inserted(_)));

        let (session_id, project, correlation, context, payload):
            (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT session_id, project, correlation, context, payload FROM events WHERE event_id = ?",
                ["01KPVSMIN01"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(session_id, None);
        assert_eq!(project, None);
        assert_eq!(correlation, None);
        assert_eq!(context, None);
        assert_eq!(payload, None);
    }
}
