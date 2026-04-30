//! Agent-activity scraper subsystem. Watches AI-coding-agent transcript files
//! on disk, parses turn boundaries, and emits `turn.metrics` events.
//!
//! Spec: docs/superpowers/specs/2026-04-30-agent-scraper-design.md
//! On by default; disable via settings key `scraper.enabled = false`.

mod state;
mod discovery;
mod emit;
mod watcher;
pub mod parsers;
pub mod pricing;

use tokio::task::JoinHandle;

/// Spawn the scraper subsystem. Returns a handle the caller may join on
/// shutdown. The subsystem owns its own state and exits cleanly when
/// the in-process broadcast channel is closed.
pub fn spawn() -> JoinHandle<()> {
    tokio::spawn(async move {
        // Wired up in Task 13.
    })
}

/// Whether the scraper should be running in this daemon process.
/// Reads `scraper.enabled` from settings; defaults to true.
pub fn is_enabled() -> bool {
    crate::config::scraper_enabled()
}
