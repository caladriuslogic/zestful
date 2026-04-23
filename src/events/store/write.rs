//! Write path for the local event store. One public function: `insert`.

use rusqlite::Connection;
use serde_json::Value;

#[derive(Debug, PartialEq)]
pub enum InsertOutcome {
    Inserted(i64),      // local rowid
    DuplicateIgnored,   // event_id already present
}

/// Insert one envelope. Returns `Inserted(rowid)` or `DuplicateIgnored`.
///
/// Uses INSERT OR IGNORE on the UNIQUE(event_id) constraint to make
/// retries idempotent.
///
/// Rejects envelopes missing any required NOT NULL field. The daemon's
/// validate_envelope should reject these upstream; we also reject here
/// so direct call sites (tests, future CLI tooling) don't silently
/// write empty-string rows.
///
/// The `rusqlite::Error::ToSqlConversionFailure` variant is reused for
/// all "malformed envelope" signals — the function returns `rusqlite::Result`
/// and no conversion-to-SQL step actually runs before this check. A
/// dedicated error type is out of scope for this task.
pub fn insert(conn: &Connection, envelope: &Value) -> rusqlite::Result<InsertOutcome> {
    let obj = envelope
        .as_object()
        .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure(
            "envelope is not an object".into(),
        ))?;

    // All NOT NULL columns are required. The daemon's validate_envelope
    // runs upstream and normally guarantees these are present, but we
    // reject here too so a direct call site can't sneak past schema.
    // ToSqlConversionFailure is reused for all "malformed envelope" signals
    // — rusqlite::Result is our return type and none of these errors
    // actually involve a SQL conversion; see the doc comment on `insert`.
    let event_id = required_str(obj, "id")?;
    let schema_version = required_i64(obj, "schema")?;
    let event_ts = required_i64(obj, "ts")?;
    let seq = required_i64(obj, "seq")?;
    let host = required_str(obj, "host")?;
    let os_user = required_str(obj, "os_user")?;
    let device_id = required_str(obj, "device_id")?;
    let source = required_str(obj, "source")?;
    let source_pid = required_i64(obj, "source_pid")?;
    let event_type = required_str(obj, "type")?;

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

/// Unix ms since epoch. Returns 0 if the system clock is pre-epoch,
/// which is a sentinel indicating clock failure — received_at will
/// be 0 in that case, distinguishable from any real modern timestamp.
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn required_str<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &str,
) -> rusqlite::Result<&'a str> {
    obj.get(key).and_then(Value::as_str).ok_or_else(|| {
        rusqlite::Error::ToSqlConversionFailure(
            format!("missing or non-string field: {}", key).into(),
        )
    })
}

fn required_i64(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> rusqlite::Result<i64> {
    obj.get(key).and_then(Value::as_i64).ok_or_else(|| {
        rusqlite::Error::ToSqlConversionFailure(
            format!("missing or non-integer field: {}", key).into(),
        )
    })
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

    #[test]
    fn insert_sets_received_at_to_approximately_now() {
        let conn = setup();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let env = fixture("01KPVSRECV01");
        insert(&conn, &env).unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let received_at: i64 = conn
            .query_row(
                "SELECT received_at FROM events WHERE event_id = ?",
                ["01KPVSRECV01"],
                |row| row.get(0),
            )
            .unwrap();

        assert!(received_at >= before, "received_at ({}) should be >= before ({})", received_at, before);
        assert!(received_at <= after, "received_at ({}) should be <= after ({})", received_at, after);
    }

    #[test]
    fn insert_roundtrips_correlation_and_payload_json() {
        let conn = setup();
        let env = fixture("01KPVSRT01");
        insert(&conn, &env).unwrap();

        let (corr_text, payload_text): (String, String) = conn
            .query_row(
                "SELECT correlation, payload FROM events WHERE event_id = ?",
                ["01KPVSRT01"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        let corr: Value = serde_json::from_str(&corr_text).unwrap();
        assert_eq!(corr["session_id"], "sess-abc");

        let payload: Value = serde_json::from_str(&payload_text).unwrap();
        assert_eq!(payload["duration_ms"], 2500);
    }

    #[test]
    fn insert_rejects_envelope_missing_required_field() {
        let conn = setup();
        // Missing "id" (required).
        let env = json!({
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
        let result = insert(&conn, &env);
        assert!(
            matches!(
                result,
                Err(rusqlite::Error::ToSqlConversionFailure(_))
            ),
            "expected ToSqlConversionFailure for missing id, got {:?}",
            result
        );

        // Verify no row was inserted.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
