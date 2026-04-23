//! `zestful events list|tail|count` — read-side of the local event store.
//! All three subcommands call events::store::query::* directly.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum EventsCommand {
    /// List events matching filters.
    List {
        /// Lower bound on received_at (unix ms).
        #[arg(long)]
        since: Option<i64>,
        /// Upper bound on received_at (unix ms).
        #[arg(long)]
        until: Option<i64>,
        #[arg(long)]
        source: Option<String>,
        /// Event type filter. Exact match unless the value contains `%`,
        /// in which case SQL LIKE is used.
        #[arg(long, value_name = "TYPE")]
        event_type: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Print JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// Follow the event stream (polls every 500 ms).
    Tail {
        #[arg(long)]
        source: Option<String>,
        #[arg(long, value_name = "TYPE")]
        event_type: Option<String>,
        /// Starting row count (most recent N events shown on first iter).
        /// Use `--initial 0` to start with no backlog and only show events
        /// that arrive after the command starts.
        #[arg(long, default_value_t = 20)]
        initial: usize,
    },
    /// Count events matching filters.
    Count {
        #[arg(long)]
        since: Option<i64>,
        #[arg(long)]
        until: Option<i64>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long, value_name = "TYPE")]
        event_type: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
}

pub fn run(command: EventsCommand) -> anyhow::Result<()> {
    // The CLI is a separate process from the daemon. Each CLI invocation
    // is a fresh process, so calling store::init() here is always the
    // first (and only) call on the OnceLock — safe. The init call opens
    // the existing DB, sets PRAGMAs (including WAL so we can read while
    // the daemon writes), and runs any pending migrations.
    let db_path = crate::config::config_dir().join("events.db");
    if !db_path.exists() {
        anyhow::bail!(
            "event store not found at {}. Is the daemon running?",
            db_path.display()
        );
    }
    crate::events::store::init(&db_path)?;

    match command {
        EventsCommand::List {
            since, until, source, event_type, session_id, agent, limit, json,
        } => run_list(since, until, source, event_type, session_id, agent, limit, json),
        EventsCommand::Tail { source, event_type, initial } => {
            run_tail(source, event_type, initial)
        }
        EventsCommand::Count {
            since, until, source, event_type, session_id, agent,
        } => run_count(since, until, source, event_type, session_id, agent),
    }
}

fn run_list(
    since: Option<i64>,
    until: Option<i64>,
    source: Option<String>,
    event_type: Option<String>,
    session_id: Option<String>,
    agent: Option<String>,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let filters = crate::events::store::query::ListFilters {
        since, until, source, event_type, session_id, agent,
    };
    let c = crate::events::store::conn().lock().unwrap();
    let (rows, _next) = crate::events::store::query::list(&c, &filters, limit, None)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!(
            "{:<20} {:<30} {:<20} {:<25} {}",
            "received_at", "event_type", "source", "session_id", "event_id"
        );
        for r in &rows {
            println!(
                "{:<20} {:<30} {:<20} {:<25} {}",
                r.received_at,
                truncate(&r.event_type, 30),
                truncate(&r.source, 20),
                truncate(r.session_id.as_deref().unwrap_or(""), 25),
                r.event_id,
            );
        }
    }
    Ok(())
}

/// Tail the event stream. Polls the store every 500 ms for rows newer
/// than `last_seen_received_at`. The per-poll fetch caps at 10_000 rows
/// (TAIL_POLL_LIMIT); bursts exceeding that rate may drop older rows in
/// the burst — a stderr warning prints when the cap is hit.
///
/// This is a monitoring and debugging tool; for loss-free historical
/// queries use `zestful events list` with explicit `--since` / `--until`.
fn run_tail(
    source: Option<String>,
    event_type: Option<String>,
    initial: usize,
) -> anyhow::Result<()> {
    // Per-poll cap. Realistic agent event streams are well below this
    // rate; at 500ms intervals this covers 20_000 events/sec. If we ever
    // hit the cap a warning prints so operators know events may have been
    // missed.
    const TAIL_POLL_LIMIT: usize = 10_000;

    let mut last_seen_received_at: i64 = if initial == 0 {
        // "No backlog, only new events." Start the watermark at the current
        // newest row so the first poll returns only events that arrive after
        // this call.
        let c = crate::events::store::conn().lock().unwrap();
        c.query_row(
            "SELECT COALESCE(MAX(received_at), 0) FROM events",
            [],
            |row| row.get::<_, i64>(0),
        )?
    } else {
        let seed_filters = crate::events::store::query::ListFilters {
            source: source.clone(),
            event_type: event_type.clone(),
            ..Default::default()
        };
        let c = crate::events::store::conn().lock().unwrap();
        let (rows, _) = crate::events::store::query::list(&c, &seed_filters, initial, None)?;
        for r in rows.iter().rev() {
            print_tail_line(r);
        }
        // list() returns DESC, so the newest row is rows[0] — first() is
        // O(1) and expresses the ordering assumption explicitly.
        rows.first().map(|r| r.received_at).unwrap_or(0)
    };

    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let poll_filters = crate::events::store::query::ListFilters {
            since: Some(last_seen_received_at + 1),
            source: source.clone(),
            event_type: event_type.clone(),
            ..Default::default()
        };
        let rows = {
            let c = crate::events::store::conn().lock().unwrap();
            let (rows, _) = crate::events::store::query::list(&c, &poll_filters, TAIL_POLL_LIMIT, None)?;
            rows
        };
        if rows.len() == TAIL_POLL_LIMIT {
            eprintln!(
                "warning: tail hit {} per-poll cap; older events in this burst may have been missed",
                TAIL_POLL_LIMIT
            );
        }
        for r in rows.iter().rev() {
            print_tail_line(r);
            if r.received_at > last_seen_received_at {
                last_seen_received_at = r.received_at;
            }
        }
    }
}

fn run_count(
    since: Option<i64>,
    until: Option<i64>,
    source: Option<String>,
    event_type: Option<String>,
    session_id: Option<String>,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let filters = crate::events::store::query::ListFilters {
        since, until, source, event_type, session_id, agent,
    };
    let c = crate::events::store::conn().lock().unwrap();
    let n = crate::events::store::query::count(&c, &filters)?;
    println!("{}", n);
    Ok(())
}

fn print_tail_line(r: &crate::events::store::query::EventRow) {
    println!(
        "{} {} [{}] session={} id={}",
        r.received_at,
        r.event_type,
        r.source,
        r.session_id.as_deref().unwrap_or("-"),
        r.event_id
    );
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{}…", cut)
}

#[cfg(test)]
mod tests {
    use crate::events::store::{schema::run_migrations, write::insert};
    use rusqlite::Connection;
    use serde_json::json;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    // The run_* functions rely on the global store connection and print
    // to stdout, so they're awkward to test directly. Verify the core
    // observable: that the query module produces the right results for
    // the flag combinations the CLI exposes.

    #[test]
    fn cli_filters_equivalent_via_query_module() {
        let conn = setup();
        for i in 0..3 {
            insert(
                &conn,
                &json!({
                    "id": format!("cli-{:03}", i),
                    "schema": 1,
                    "ts": 1_000 + i,
                    "seq": 0,
                    "host": "h",
                    "os_user": "u",
                    "device_id": "d",
                    "source": "claude-code",
                    "source_pid": 1,
                    "type": "turn.completed"
                }),
            ).unwrap();
        }
        let filters = crate::events::store::query::ListFilters {
            source: Some("claude-code".into()),
            ..Default::default()
        };
        let (rows, _) = crate::events::store::query::list(&conn, &filters, 10, None).unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn truncate_handles_short_and_long() {
        use super::truncate;
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly10c", 10), "exactly10c");
        assert_eq!(truncate("this-is-way-too-long", 10), "this-is-w…");

        // Multi-byte UTF-8: each char is 3 bytes, count by chars not bytes.
        // 5 chars total, n=10 → no truncation. No panic at byte boundary.
        assert_eq!(truncate("日本語テスト", 10), "日本語テスト");
        // 5 chars, n=3 → take 2 + ellipsis.
        assert_eq!(truncate("日本語テスト", 3), "日本…");
    }
}
