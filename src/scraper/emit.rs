//! Build envelopes from TurnRecords and submit them via the daemon's
//! existing in-process insert + broadcast path. Same code path the
//! HTTP /events handler uses, minus the HTTP layer.

use crate::events::payload::{Payload, TurnContext, TurnMetrics, TurnTokens};
use crate::scraper::parsers::TurnRecord;
use crate::scraper::pricing;

/// Build the wire-format envelope (as `serde_json::Value`) for one
/// TurnRecord. Pure: no I/O, no clock, no global state. The deterministic
/// envelope id makes re-emits idempotent at the store dedup layer.
///
/// `agent` is the agent label classified by the dispatch loop from the
/// transcript path's root (e.g. "claude-code" or "codex").
pub fn build_envelope(rec: &TurnRecord, agent: &str) -> serde_json::Value {
    let max = pricing::context_window_of(&rec.model);
    let cost = pricing::cost_estimate_usd(&rec.model, &rec.tokens);

    let used = rec.tokens.input;  // current context size = this turn's input tokens
    let ratio = max.map(|m| if m == 0 { 0.0 } else { used as f64 / m as f64 });

    let payload = Payload::TurnMetrics(TurnMetrics {
        model: rec.model.clone(),
        tokens: TurnTokens {
            input: rec.tokens.input,
            output: rec.tokens.output,
            cache_read: rec.tokens.cache_read,
            cache_write: rec.tokens.cache_write,
            reasoning: rec.tokens.reasoning,
        },
        context: TurnContext {
            used_tokens: used,
            max_tokens: max,
            ratio,
        },
        cost_estimate_usd: cost,
        message_count: rec.message_count,
    });

    let id = deterministic_envelope_id(rec.ts_ms, "agent-scraper", &rec.session_id, &rec.turn_id);

    serde_json::json!({
        "id": id,
        "schema": 1,
        "ts": rec.ts_ms.max(0),
        "seq": 0,
        "host": crate::events::device::host(),
        "os_user": crate::events::device::os_user(),
        "device_id": crate::events::device::device_id(),
        "source": "agent-scraper",
        "source_pid": std::process::id(),
        "type": "turn.metrics",
        "correlation": {
            "session_id": rec.session_id,
            "turn_id": rec.turn_id,
        },
        "context": {
            "agent": agent,
            "model": rec.model,
        },
        "payload": payload.to_body_value(),
    })
}

/// Build a deterministic 26-char ULID-format id from immutable turn data.
/// Same inputs always produce the same id, so re-emits get rejected by
/// the store's `event_id` unique constraint.
pub fn deterministic_envelope_id(
    ts_ms: i64,
    source: &str,
    session_id: &str,
    turn_id: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    h.update(b"|");
    h.update(session_id.as_bytes());
    h.update(b"|");
    h.update(turn_id.as_bytes());
    let digest = h.finalize();

    // Take the first 10 bytes (80 bits) as ULID's "random" component.
    let mut random = [0u8; 10];
    random.copy_from_slice(&digest[..10]);

    // Non-negative ms-epoch for ULID's time component (48 bits).
    let ms = if ts_ms < 0 { 0 } else { ts_ms as u64 };
    let ulid = ulid::Ulid::from_parts(ms, u128::from_be_bytes({
        let mut b = [0u8; 16];
        b[6..].copy_from_slice(&random); // pack 80 random bits into low 10 bytes
        b
    }));
    ulid.to_string()
}

/// Emit one envelope through the daemon's existing insert + broadcast path.
/// Synchronous DB write is wrapped by the caller in spawn_blocking.
///
/// Mirrors the work the HTTP /events handler does in cmd/daemon.rs minus
/// validate_envelope (we trust build_envelope's output): insert, fire the
/// prune-check counter, broadcast. Without the prune-check call, scraper
/// writes wouldn't tick WRITE_COUNTER and the events table would grow
/// unbounded on heavy users where scraper traffic dominates HTTP traffic.
pub fn submit_envelope(env: &serde_json::Value) -> rusqlite::Result<()> {
    let conn = crate::events::store::conn().lock().unwrap();
    let outcome = crate::events::store::write::insert(&conn, env)?;
    drop(conn);

    crate::events::store::record_insert_and_maybe_prune(
        crate::events::store::DEFAULT_MAX_BYTES,
    );

    let event_type = env.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    crate::events::broadcast::send(crate::events::broadcast::ProjectionChangedFrame {
        source_event_types: vec![event_type],
        ts: now_ms,
        reason: None,
    });

    let _ = outcome; // dedup duplicates are not an error condition
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scraper::parsers::Tokens;

    fn rec() -> TurnRecord {
        TurnRecord {
            session_id: "sess_abc".into(),
            turn_id: "turn_xyz".into(),
            model: "claude-3-5-sonnet-20241022".into(),
            tokens: Tokens { input: 12450, output: 832, cache_read: 8120, cache_write: 0, reasoning: 0 },
            ts_ms: 1761830000000,
            message_count: 1,
        }
    }

    #[test]
    fn deterministic_id_is_stable() {
        let a = deterministic_envelope_id(1, "agent-scraper", "s1", "t1");
        let b = deterministic_envelope_id(1, "agent-scraper", "s1", "t1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 26, "must be 26-char ULID");
    }

    #[test]
    fn deterministic_id_changes_with_inputs() {
        let a = deterministic_envelope_id(1, "agent-scraper", "s1", "t1");
        let b = deterministic_envelope_id(1, "agent-scraper", "s1", "t2");
        assert_ne!(a, b);
    }

    #[test]
    fn envelope_has_required_top_level_fields() {
        let env = build_envelope(&rec(), "claude-code");
        for f in &["id","schema","ts","seq","host","os_user","device_id","source","source_pid","type"] {
            assert!(env.get(*f).is_some(), "missing field {}", f);
        }
        assert_eq!(env["type"], "turn.metrics");
        assert_eq!(env["source"], "agent-scraper");
        assert_eq!(env["schema"], 1);
        assert_eq!(env["context"]["agent"], "claude-code");
    }

    #[test]
    fn payload_carries_all_metrics() {
        let env = build_envelope(&rec(), "claude-code");
        let p = &env["payload"];
        assert_eq!(p["model"], "claude-3-5-sonnet-20241022");
        assert_eq!(p["tokens"]["input"], 12450);
        assert_eq!(p["tokens"]["output"], 832);
        assert_eq!(p["tokens"]["cache_read"], 8120);
        assert_eq!(p["context"]["used_tokens"], 12450);
        assert_eq!(p["context"]["max_tokens"], 200000);
        assert!(p["context"]["ratio"].as_f64().unwrap() > 0.0);
        assert!(p["cost_estimate_usd"].as_f64().unwrap() > 0.0);
        assert_eq!(p["message_count"], 1);
    }

    #[test]
    fn unknown_model_yields_null_max_and_ratio_and_cost() {
        let mut r = rec();
        r.model = "totally-made-up".into();
        let env = build_envelope(&r, "claude-code");
        let p = &env["payload"];
        assert_eq!(p["context"]["used_tokens"], 12450);
        assert!(p["context"]["max_tokens"].is_null());
        assert!(p["context"]["ratio"].is_null());
        assert!(p["cost_estimate_usd"].is_null());
    }
}
