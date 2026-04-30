//! Claude Code transcript parser. Reads `~/.claude/projects/*/*.jsonl`
//! files. Each line is one envelope; turns are identified by lines
//! with `type == "assistant"` carrying a `message.usage` block.

use super::{ParseResult, Parser, TurnRecord, Tokens};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

pub struct ClaudeParser;

impl Parser for ClaudeParser {
    fn agent(&self) -> &'static str {
        "claude-code"
    }

    fn parse_from(
        &self,
        path: &Path,
        from_offset: u64,
    ) -> std::io::Result<ParseResult> {
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(from_offset))?;
        let mut reader = BufReader::new(f);

        let mut records = Vec::new();
        let mut consumed: u64 = 0;
        let mut last_complete_offset = from_offset;

        loop {
            let mut buf = String::new();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break; // EOF
            }
            consumed += n as u64;

            // A line is "complete" only if it ended with a newline.
            // read_line includes the newline if present; partial trailing
            // lines come back without one, which means we should NOT
            // advance last_complete_offset past it.
            let ended_with_newline = buf.ends_with('\n');
            if !ended_with_newline {
                break;
            }

            // Try to parse this line. Bad lines are silently skipped
            // but the offset still advances past them.
            if let Some(rec) = parse_line(&buf) {
                records.push(rec);
            }
            last_complete_offset = from_offset + consumed;
        }

        Ok(ParseResult { records, last_complete_offset })
    }
}

fn parse_line(line: &str) -> Option<TurnRecord> {
    let v: Value = serde_json::from_str(line.trim_end()).ok()?;

    // Only `type == "assistant"` lines with a usage block contribute turns.
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;

    let session_id = v.get("sessionId")?.as_str()?.to_string();
    let turn_id = msg.get("id")?.as_str()?.to_string();
    let model = msg.get("model")?.as_str()?.to_string();

    let tokens = Tokens {
        input: u(usage, "input_tokens"),
        output: u(usage, "output_tokens"),
        cache_read: u(usage, "cache_read_input_tokens"),
        cache_write: u(usage, "cache_creation_input_tokens"),
        reasoning: u(usage, "reasoning_tokens"),
    };

    let ts_ms = v.get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(parse_iso8601_ms)
        .unwrap_or(0);

    Some(TurnRecord {
        session_id,
        turn_id,
        model,
        tokens,
        ts_ms,
        message_count: 1,
    })
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Minimal ISO-8601 -> ms-epoch parser. Returns None on anything we don't
/// recognize. We don't pull in chrono just for this; the daemon already
/// works in raw ms epochs everywhere.
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
    let frac_ms: u32 = sf_parts.next()
        .map(|f| {
            let f3 = format!("{:0<3}", &f[..f.len().min(3)]);
            f3.parse::<u32>().unwrap_or(0)
        })
        .unwrap_or(0);

    // Days since unix epoch — naive Gregorian calculation.
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
            .join("tests/fixtures/scraper/claude")
            .join(name)
    }

    #[test]
    fn parses_complete_turn() {
        let p = ClaudeParser;
        let r = p.parse_from(&fixture("turn_complete.jsonl"), 0).unwrap();
        assert!(!r.records.is_empty(), "should parse at least one turn");
        let first = &r.records[0];
        assert!(!first.session_id.is_empty());
        assert!(!first.turn_id.is_empty());
        assert!(first.model.starts_with("claude-"));
        assert!(first.tokens.output > 0);
        assert!(r.last_complete_offset > 0);
    }

    #[test]
    fn skips_partial_last_line() {
        let p = ClaudeParser;
        let full = p.parse_from(&fixture("turn_complete.jsonl"), 0).unwrap();
        let partial = p.parse_from(&fixture("partial_last_line.jsonl"), 0).unwrap();
        // Same complete records as the un-truncated file.
        assert_eq!(partial.records, full.records);
        // Offset stops at the last newline, NOT at EOF of the partial file.
        assert_eq!(partial.last_complete_offset, full.last_complete_offset);
    }

    #[test]
    fn skips_malformed_line_in_middle() {
        let p = ClaudeParser;
        let r = p.parse_from(&fixture("malformed_middle.jsonl"), 0).unwrap();
        // Bad line is dropped silently; remaining good lines still parse.
        assert!(!r.records.is_empty());
    }

    #[test]
    fn resumes_from_offset() {
        let p = ClaudeParser;
        let full = p.parse_from(&fixture("turn_complete.jsonl"), 0).unwrap();
        // The fixture begins with user lines (which produce no records);
        // skipping past the first newline alone wouldn't drop any
        // assistant turns. Resume from the start of the last line so we
        // parse strictly fewer records than the full file.
        let bytes = std::fs::read(fixture("turn_complete.jsonl")).unwrap();
        let nls: Vec<usize> = bytes.iter().enumerate()
            .filter(|(_, &b)| b == b'\n').map(|(i, _)| i).collect();
        let resume_offset = (nls[nls.len() - 2] + 1) as u64;
        let resume = p.parse_from(&fixture("turn_complete.jsonl"),
                                  resume_offset).unwrap();
        // Should yield strictly fewer records than the full parse.
        assert!(resume.records.len() < full.records.len());
        // Final offset matches the full parse.
        assert_eq!(resume.last_complete_offset, full.last_complete_offset);
    }
}
