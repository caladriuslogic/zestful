//! Summary projection — globals derived from turn.metrics events.
//! See spec: docs/superpowers/specs/2026-05-01-zestful-top-metrics-design.md.

pub mod sql;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Summary {
    /// Σ cost_estimate_usd for turn.metrics where ts ≥ today midnight (local).
    /// Excludes events whose model has no pricing entry.
    pub today_cost_usd: f64,
    /// Σ (input + output + cache_read + cache_write + reasoning)
    /// for turn.metrics where ts ≥ today midnight (local).
    pub today_tokens: u64,
    /// distinct context.agent over the today window.
    pub agents: u32,
    /// distinct correlation.session_id over the today window.
    pub sessions: u32,
    /// 7 buckets of ≈3.4 h each, summing cost_estimate_usd.
    /// `cost_sparkline[6]` is the bucket containing now.
    pub cost_sparkline: [f64; 7],
}

/// Compute the summary projection at a given wall-clock instant. Pure
/// function over the events table — no caching. The macOS app and the
/// `zestful top` TUI both consume this via `GET /summary`.
///
/// `now_ms` is passed in (rather than read from the clock) for testability.
pub fn compute(conn: &Connection, now_ms: i64) -> rusqlite::Result<Summary> {
    let today_start = sql::today_window_ms(now_ms);
    let sparkline_start = now_ms - 24 * 3_600_000;

    // Today aggregates: scan one query, fold all four scalars in one pass.
    let mut today_cost = 0.0f64;
    let mut today_tokens = 0u64;
    let mut agents: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut sessions: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut stmt = conn.prepare(sql::TODAY_METRICS_SQL)?;
    let rows = stmt.query_map([today_start], |row| {
        let ctx_str: Option<String> = row.get(0)?;
        let payload_str: Option<String> = row.get(1)?;
        Ok((ctx_str, payload_str))
    })?;
    for r in rows {
        let (ctx_str, payload_str) = r?;
        if let (Some(c), Some(p)) = (ctx_str, payload_str) {
            let ctx: serde_json::Value = match serde_json::from_str(&c) {
                Ok(v) => v, Err(_) => continue,
            };
            let payload: serde_json::Value = match serde_json::from_str(&p) {
                Ok(v) => v, Err(_) => continue,
            };
            if let Some(a) = ctx.get("agent").and_then(|v| v.as_str()) {
                agents.insert(a.to_string());
            }
            if let Some(c) = payload.get("cost_estimate_usd").and_then(|v| v.as_f64()) {
                today_cost += c;
            }
            if let Some(t) = payload.get("tokens").and_then(|v| v.as_object()) {
                let sum = ["input","output","cache_read","cache_write","reasoning"]
                    .iter()
                    .filter_map(|k| t.get(*k).and_then(|v| v.as_u64()))
                    .sum::<u64>();
                today_tokens += sum;
            }
        }
    }
    drop(stmt);

    // Distinct sessions — separate query against the indexed session_id column.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT session_id FROM events
         WHERE event_type = 'turn.metrics'
           AND event_ts >= ? AND session_id IS NOT NULL"
    )?;
    let sess_rows = stmt.query_map([today_start], |row| row.get::<_, String>(0))?;
    for r in sess_rows {
        sessions.insert(r?);
    }
    drop(stmt);

    // Sparkline buckets.
    let mut buckets = [0.0f64; 7];
    let mut stmt = conn.prepare(sql::SPARKLINE_METRICS_SQL)?;
    let rows = stmt.query_map([sparkline_start], |row| {
        let ts: i64 = row.get(0)?;
        let payload_str: Option<String> = row.get(1)?;
        Ok((ts, payload_str))
    })?;
    for r in rows {
        let (ts, payload_str) = r?;
        if let Some(p) = payload_str {
            let payload: serde_json::Value = match serde_json::from_str(&p) {
                Ok(v) => v, Err(_) => continue,
            };
            if let Some(c) = payload.get("cost_estimate_usd").and_then(|v| v.as_f64()) {
                if let Some(b) = sql::bucket_idx(ts, now_ms) {
                    buckets[b] += c;
                }
            }
        }
    }

    Ok(Summary {
        today_cost_usd: today_cost,
        today_tokens,
        agents: agents.len() as u32,
        sessions: sessions.len() as u32,
        cost_sparkline: buckets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_default_is_zero() {
        let s = Summary::default();
        assert_eq!(s.today_cost_usd, 0.0);
        assert_eq!(s.today_tokens, 0);
        assert_eq!(s.agents, 0);
        assert_eq!(s.sessions, 0);
        assert_eq!(s.cost_sparkline, [0.0; 7]);
    }

    #[test]
    fn summary_roundtrip() {
        let s = Summary {
            today_cost_usd: 4.27,
            today_tokens: 142_300,
            agents: 3,
            sessions: 7,
            cost_sparkline: [0.1, 0.2, 0.4, 0.3, 0.5, 0.8, 1.2],
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: Summary = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}

#[cfg(test)]
mod compute_tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    fn open_memory_with_schema() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::events::store::schema::run_migrations(&conn).unwrap();
        conn
    }

    fn insert_metric(conn: &Connection, event_id: &str, ts_ms: i64,
                     agent: &str, session_id: &str, cost: f64,
                     input: u64, output: u64, cache_read: u64) {
        let context = json!({ "agent": agent, "model": "claude-3-5-sonnet-20241022" });
        let correlation = json!({ "session_id": session_id, "turn_id": event_id });
        let payload = json!({
            "model": "claude-3-5-sonnet-20241022",
            "tokens": { "input": input, "output": output, "cache_read": cache_read,
                        "cache_write": 0, "reasoning": 0 },
            "context": { "used_tokens": input, "max_tokens": 200000,
                         "ratio": (input as f64) / 200_000.0 },
            "cost_estimate_usd": cost,
            "message_count": 1
        });
        conn.execute(
            "INSERT INTO events (
                received_at, event_id, schema_version, event_ts, seq,
                host, os_user, device_id, source, source_pid,
                event_type, session_id, project, correlation, context, payload
            ) VALUES (?,?,1,?,0,'h','u','d','agent-scraper',1,
                      'turn.metrics',?,NULL,?,?,?)",
            rusqlite::params![
                ts_ms, event_id, ts_ms, session_id,
                correlation.to_string(), context.to_string(), payload.to_string(),
            ],
        ).unwrap();
    }

    #[test]
    fn empty_db_yields_zero_summary() {
        let conn = open_memory_with_schema();
        let s = compute(&conn, 1_761_830_000_000).unwrap();
        assert_eq!(s, Summary::default());
    }

    #[test]
    fn computes_today_cost_today_tokens_and_counts() {
        let conn = open_memory_with_schema();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        insert_metric(&conn, "e1", now - 1000, "claude-code", "s1", 0.10, 1000, 200, 0);
        insert_metric(&conn, "e2", now - 2000, "claude-code", "s2", 0.20, 2000, 300, 100);
        insert_metric(&conn, "e3", now - 3000, "codex",       "s3", 0.05, 500,  50,  0);
        // One event yesterday — must be excluded from "today".
        insert_metric(&conn, "e0", now - 26 * 3_600_000, "claude-code", "s9",
                      99.99, 100, 100, 100);

        let s = compute(&conn, now).unwrap();
        assert!((s.today_cost_usd - 0.35).abs() < 1e-9);
        assert_eq!(s.today_tokens, 1000+200 + 2000+300+100 + 500+50);
        assert_eq!(s.agents, 2);
        assert_eq!(s.sessions, 3);
    }

    #[test]
    fn cost_sparkline_buckets_24h_correctly() {
        let conn = open_memory_with_schema();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let span = 24 * 3_600_000;
        let bucket = span / 7;
        for i in 0..7 {
            // Middle of bucket i.
            let offset = i * bucket + bucket / 2;
            let ts = (now - span) + offset;
            insert_metric(&conn, &format!("e{}", i), ts, "claude-code",
                          &format!("s{}", i), (i + 1) as f64 * 0.1, 100, 0, 0);
        }
        let s = compute(&conn, now).unwrap();
        for i in 0..7 {
            let expected = (i + 1) as f64 * 0.1;
            assert!((s.cost_sparkline[i] - expected).abs() < 1e-9,
                    "bucket {} expected {} got {}", i, expected, s.cost_sparkline[i]);
        }
    }

    #[test]
    fn missing_cost_estimate_excluded_from_today_cost_but_counted_in_tokens() {
        let conn = open_memory_with_schema();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let context = json!({ "agent": "claude-code", "model": "totally-made-up" });
        let correlation = json!({ "session_id": "s1", "turn_id": "e1" });
        let payload = json!({
            "model": "totally-made-up",
            "tokens": { "input": 1000, "output": 200, "cache_read": 0,
                        "cache_write": 0, "reasoning": 0 },
            "context": { "used_tokens": 1000, "max_tokens": null, "ratio": null },
            "cost_estimate_usd": null,
            "message_count": 1
        });
        conn.execute(
            "INSERT INTO events (
                received_at, event_id, schema_version, event_ts, seq,
                host, os_user, device_id, source, source_pid,
                event_type, session_id, project, correlation, context, payload
            ) VALUES (?,?,1,?,0,'h','u','d','agent-scraper',1,
                      'turn.metrics',?,NULL,?,?,?)",
            rusqlite::params![
                now - 1000, "e1", now - 1000, "s1",
                correlation.to_string(), context.to_string(), payload.to_string(),
            ],
        ).unwrap();
        let s = compute(&conn, now).unwrap();
        assert_eq!(s.today_cost_usd, 0.0);
        assert_eq!(s.today_tokens, 1200);
        assert_eq!(s.sessions, 1);
        assert_eq!(s.agents, 1);
    }
}
