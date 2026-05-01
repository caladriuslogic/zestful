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
pub fn compute(_conn: &Connection, _now_ms: i64) -> rusqlite::Result<Summary> {
    // Implemented in Task 4. Stub returns Default for now so dependent
    // wiring tasks can compile.
    Ok(Summary::default())
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
