//! Summary projection — globals derived from turn.metrics events.
//! See spec: docs/superpowers/specs/2026-05-01-zestful-top-metrics-design.md.

pub mod summary;
pub mod sql;

pub use summary::Summary;

use rusqlite::Connection;

/// Compute the summary projection at a given wall-clock instant. Pure
/// function over the events table — no caching. The macOS app and the
/// `zestful top` TUI both consume this via `GET /summary`.
///
/// `now_ms` is passed in (rather than read from the clock) for testability.
#[allow(unused_variables)]
pub fn compute(conn: &Connection, now_ms: i64) -> rusqlite::Result<Summary> {
    // Implemented in Task 4. Stub returns Default for now so dependent
    // wiring tasks can compile.
    Ok(Summary::default())
}
