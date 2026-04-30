//! Codex transcript parser. Reads
//! `~/.codex/sessions/<year>/<month>/<day>/rollout-<isoTime>-<uuid>.jsonl`
//! files. Each line is one envelope `{timestamp, type, payload}`.
//!
//! Compared to Claude:
//!   - `session_id` lives in a single `session_meta` header line at the
//!     top of the file (not on every line). Resuming from a non-zero
//!     offset must re-scan from byte 0 to recover it.
//!   - `model` and `turn_id` live on `turn_context` lines and are
//!     applied to subsequent `token_count` events for that turn.
//!   - Token usage comes from `event_msg` lines with
//!     `payload.type == "token_count"`, under `payload.info.last_token_usage`.
//!     `event_msg` lines with `payload.info == null` (placeholders early
//!     in a session) are skipped.

use super::{ParseResult, Parser, Tokens, TurnRecord};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

pub struct CodexParser;

impl Parser for CodexParser {
    fn agent(&self) -> &'static str {
        "codex"
    }

    fn parse_from(
        &self,
        path: &Path,
        from_offset: u64,
    ) -> std::io::Result<ParseResult> {
        // Recover session_id from byte 0 of the file. We re-scan from the
        // start every parse: the header is one short line, and we'd
        // otherwise need to thread session_id through `FileState`. If the
        // header is missing or unparseable, no records will be emitted.
        let session_id = read_session_id(path)?.unwrap_or_default();

        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(from_offset))?;
        let mut reader = BufReader::new(f);

        let mut records = Vec::new();
        let mut consumed: u64 = 0;
        let mut last_complete_offset = from_offset;

        let mut current_model = String::new();
        let mut current_turn_id = String::new();

        loop {
            let mut buf = String::new();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break; // EOF
            }
            consumed += n as u64;

            // A line is "complete" only if it ended with a newline.
            // Same convention as the Claude parser: partial trailing
            // line does NOT advance last_complete_offset.
            let ended_with_newline = buf.ends_with('\n');
            if !ended_with_newline {
                break;
            }

            if let Some(rec) = parse_line(
                &buf,
                &session_id,
                &mut current_model,
                &mut current_turn_id,
            ) {
                records.push(rec);
            }
            last_complete_offset = from_offset + consumed;
        }

        Ok(ParseResult { records, last_complete_offset })
    }
}

/// Read the first line of the file and, if it's a `session_meta`
/// envelope, return `payload.id`. Returns `Ok(None)` for any malformed
/// or non-session_meta first line.
fn read_session_id(path: &Path) -> std::io::Result<Option<String>> {
    let f = File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut buf = String::new();
    let n = reader.read_line(&mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    let v: Value = match serde_json::from_str(buf.trim_end()) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return Ok(None);
    }
    Ok(v.get("payload")
        .and_then(|p| p.get("id"))
        .and_then(|i| i.as_str())
        .map(|s| s.to_string()))
}

/// Parse one transcript line. Updates `current_model` / `current_turn_id`
/// when a `turn_context` line is encountered, and emits a `TurnRecord`
/// when a `token_count` event with non-null `info` is encountered.
fn parse_line(
    line: &str,
    session_id: &str,
    current_model: &mut String,
    current_turn_id: &mut String,
) -> Option<TurnRecord> {
    let v: Value = serde_json::from_str(line.trim_end()).ok()?;

    let ty = v.get("type")?.as_str()?;
    let payload = v.get("payload")?;

    match ty {
        "turn_context" => {
            if let Some(m) = payload.get("model").and_then(|x| x.as_str()) {
                *current_model = m.to_string();
            }
            if let Some(t) = payload.get("turn_id").and_then(|x| x.as_str()) {
                *current_turn_id = t.to_string();
            }
            None
        }
        "event_msg" => {
            // Only token_count events with non-null info contribute turns.
            let inner_ty = payload.get("type").and_then(|x| x.as_str())?;
            if inner_ty != "token_count" {
                return None;
            }
            let info = payload.get("info")?;
            if info.is_null() {
                return None;
            }
            let last = info.get("last_token_usage")?;

            // Need at least a session_id and a model to attribute the
            // record. (Per spec: skip if no turn_context has been seen.)
            if session_id.is_empty() || current_model.is_empty() {
                return None;
            }

            let tokens = Tokens {
                input: u(last, "input_tokens"),
                output: u(last, "output_tokens"),
                cache_read: u(last, "cached_input_tokens"),
                cache_write: 0, // Codex has no cache_creation analog.
                reasoning: u(last, "reasoning_output_tokens"),
            };

            let ts_ms = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601_ms)
                .unwrap_or(0);

            // Prefer a turn_id carried on this line; otherwise the most
            // recent turn_context's turn_id; otherwise a deterministic
            // hash so the dispatch loop can dedupe.
            let line_turn_id = payload
                .get("turn_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let turn_id = line_turn_id
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    if current_turn_id.is_empty() {
                        None
                    } else {
                        Some(current_turn_id.clone())
                    }
                })
                .unwrap_or_else(|| derive_turn_id(session_id, current_model, &tokens, ts_ms));

            Some(TurnRecord {
                session_id: session_id.to_string(),
                turn_id,
                model: current_model.clone(),
                tokens,
                ts_ms,
                message_count: 1,
            })
        }
        // session_meta, response_item, anything else: skip.
        _ => None,
    }
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Stable turn_id when no `turn_context.turn_id` is available. Derived
/// from immutable per-turn data — including ts_ms — so a re-parse of the
/// same line yields the same id, but two consecutive turns with identical
/// token counts (round-numbered cached prompts are common) don't collide
/// and silently merge. The dispatch loop dedupes on (session_id, turn_id);
/// a collision here would silently drop a turn.
fn derive_turn_id(session_id: &str, model: &str, t: &Tokens, ts_ms: i64) -> String {
    let mut h = Sha256::new();
    h.update(session_id.as_bytes());
    h.update(b"|");
    h.update(model.as_bytes());
    h.update(b"|");
    h.update(ts_ms.to_le_bytes());
    h.update(b"|");
    h.update(t.input.to_le_bytes());
    h.update(t.output.to_le_bytes());
    h.update(t.cache_read.to_le_bytes());
    h.update(t.reasoning.to_le_bytes());
    let digest = h.finalize();
    // 16 hex chars = 64 bits of digest, plenty for an in-process dedup key.
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// Minimal ISO-8601 -> ms-epoch parser. Same shape as Claude parser.
/// (Copied here rather than extracted; a follow-up can dedupe.)
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    // Format: 2026-04-30T12:34:56.789Z
    let s = s.strip_suffix('Z')?;
    let mut parts = s.split('T');
    let date = parts.next()?;
    let time = parts.next()?;
    let mut d = date.split('-');
    let y: i32 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let dy: u32 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: u32 = t.next()?.parse().ok()?;
    let mi: u32 = t.next()?.parse().ok()?;
    let sf = t.next()?;
    let mut sf_parts = sf.split('.');
    let s_int: u32 = sf_parts.next()?.parse().ok()?;
    let frac_ms: u32 = sf_parts
        .next()
        .map(|f| {
            let f3 = format!("{:0<3}", &f[..f.len().min(3)]);
            f3.parse::<u32>().unwrap_or(0)
        })
        .unwrap_or(0);

    let days = days_from_civil(y, mo, dy);
    let secs = (days as i64) * 86_400 + (h as i64) * 3600 + (mi as i64) * 60 + s_int as i64;
    Some(secs * 1000 + frac_ms as i64)
}

/// Hinnant's algorithm for days from civil date to unix epoch.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5
        + (d as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scraper::parsers::Parser;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scraper/codex")
            .join(name)
    }

    #[test]
    fn parses_complete_turn() {
        let p = CodexParser;
        let r = p.parse_from(&fixture("turn_complete.jsonl"), 0).unwrap();
        assert!(!r.records.is_empty(), "should parse at least one turn");
        let first = &r.records[0];
        assert!(!first.session_id.is_empty(), "session_id from session_meta header");
        assert!(!first.turn_id.is_empty());
        assert!(!first.model.is_empty(), "model from turn_context");
        assert!(first.tokens.output > 0);
        assert!(first.tokens.input > 0);
    }

    #[test]
    fn skips_partial_last_line() {
        let p = CodexParser;
        let full = p.parse_from(&fixture("turn_complete.jsonl"), 0).unwrap();
        let partial = p.parse_from(&fixture("partial_last_line.jsonl"), 0).unwrap();
        assert_eq!(partial.records, full.records);
        assert_eq!(partial.last_complete_offset, full.last_complete_offset);
    }

    #[test]
    fn resumes_recovers_session_id() {
        // Resume from past the session_meta header line. The parser must
        // still extract session_id by re-reading byte 0 of the file, so
        // any records produced have non-empty session_id.
        let p = CodexParser;
        let bytes = std::fs::read(fixture("turn_complete.jsonl")).unwrap();
        let first_nl = bytes.iter().position(|&b| b == b'\n').unwrap();
        let resume = p.parse_from(&fixture("turn_complete.jsonl"),
                                  (first_nl + 1) as u64).unwrap();
        // Guard against a regression that empties records — the loop below
        // would otherwise vacuously pass.
        assert!(
            !resume.records.is_empty(),
            "resume after header should still emit a turn"
        );
        for r in &resume.records {
            assert!(!r.session_id.is_empty(),
                    "session_id must be recovered from header even after resume");
        }
    }

    /// Synthetic-line tests that exercise the four documented skip rules
    /// directly via parse_line, without needing a fixture file. These
    /// catch regressions in the dispatch logic that the fixture-based
    /// tests don't cover (the fixture happens to contain only the happy
    /// path plus one info-bearing token_count line).
    mod skip_rules {
        use super::super::*;
        use crate::scraper::parsers::Tokens;
        use serde_json::json;

        fn run(line: serde_json::Value) -> Option<TurnRecord> {
            let mut model = String::new();
            let mut turn_id = String::new();
            let s = line.to_string() + "\n";
            parse_line(&s, "sess_test", &mut model, &mut turn_id)
        }

        fn run_with_ctx(line: serde_json::Value, model: &str) -> Option<TurnRecord> {
            let mut m = model.to_string();
            let mut t = String::new();
            let s = line.to_string() + "\n";
            parse_line(&s, "sess_test", &mut m, &mut t)
        }

        #[test]
        fn skips_response_item() {
            let line = json!({"timestamp": "2026-04-30T00:00:00.000Z", "type": "response_item",
                              "payload": {"type": "message", "content": "hi"}});
            assert!(run(line).is_none());
        }

        #[test]
        fn skips_event_msg_non_token_count() {
            let line = json!({"timestamp": "2026-04-30T00:00:00.000Z", "type": "event_msg",
                              "payload": {"type": "thread_id", "thread_id": "abc"}});
            assert!(run(line).is_none());
        }

        #[test]
        fn skips_token_count_with_null_info() {
            let line = json!({"timestamp": "2026-04-30T00:00:00.000Z", "type": "event_msg",
                              "payload": {"type": "token_count", "info": null}});
            assert!(run_with_ctx(line, "gpt-5.4").is_none());
        }

        #[test]
        fn skips_token_count_before_any_turn_context() {
            // current_model is empty → skip even if info is present.
            let line = json!({"timestamp": "2026-04-30T00:00:00.000Z", "type": "event_msg",
                              "payload": {"type": "token_count", "info": {
                                  "last_token_usage": {"input_tokens": 100, "output_tokens": 50,
                                                       "cached_input_tokens": 0, "reasoning_output_tokens": 0}}}});
            assert!(run(line).is_none());
        }

        #[test]
        fn turn_context_updates_state() {
            let mut model = String::new();
            let mut turn_id = String::new();
            let line = json!({"timestamp": "2026-04-30T00:00:00.000Z", "type": "turn_context",
                              "payload": {"model": "gpt-5.4", "turn_id": "t-123"}});
            let s = line.to_string() + "\n";
            let rec = parse_line(&s, "sess_test", &mut model, &mut turn_id);
            // turn_context never produces a record itself.
            assert!(rec.is_none());
            // But it updates the carried state.
            assert_eq!(model, "gpt-5.4");
            assert_eq!(turn_id, "t-123");
        }

        #[test]
        fn token_count_with_info_after_turn_context_emits_record() {
            let mut model = "gpt-5.4".to_string();
            let mut turn_id = "t-123".to_string();
            let line = json!({"timestamp": "2026-04-30T00:00:00.000Z", "type": "event_msg",
                              "payload": {"type": "token_count", "info": {
                                  "last_token_usage": {"input_tokens": 100, "output_tokens": 50,
                                                       "cached_input_tokens": 10, "reasoning_output_tokens": 5}}}});
            let s = line.to_string() + "\n";
            let rec = parse_line(&s, "sess_test", &mut model, &mut turn_id).unwrap();
            assert_eq!(rec.session_id, "sess_test");
            assert_eq!(rec.turn_id, "t-123");
            assert_eq!(rec.model, "gpt-5.4");
            assert_eq!(rec.tokens, Tokens { input: 100, output: 50, cache_read: 10, cache_write: 0, reasoning: 5 });
        }

        #[test]
        fn deterministic_turn_id_includes_ts_ms() {
            // Two turns with identical token counts but different timestamps
            // must produce different turn_ids — otherwise the dispatch loop
            // silently merges them.
            let t = Tokens { input: 100, output: 50, cache_read: 0, cache_write: 0, reasoning: 0 };
            let a = derive_turn_id("s", "m", &t, 1_000_000);
            let b = derive_turn_id("s", "m", &t, 1_000_001);
            assert_ne!(a, b, "ts_ms must contribute to derive_turn_id");
        }
    }
}
