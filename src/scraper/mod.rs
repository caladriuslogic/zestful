//! Agent-activity scraper subsystem. See module submodules for
//! component-level docs and the spec for design rationale.
//!
//! Spec: docs/superpowers/specs/2026-04-30-agent-scraper-design.md

mod discovery;
mod emit;
mod state;
mod watcher;

pub mod parsers;
pub mod pricing;

use crate::scraper::parsers::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Whether the scraper should be running in this daemon process.
pub fn is_enabled() -> bool {
    crate::config::scraper_enabled()
}

/// Spawn the scraper. Returns a JoinHandle that completes when the
/// channel from the watcher closes (typically: daemon shutdown).
pub fn spawn() -> tokio::task::JoinHandle<()> {
    let roots = discovery::resolve_roots();
    let root_paths: Vec<PathBuf> = roots.iter().map(|r| r.path.clone()).collect();

    // Map root path -> agent label. Used to classify FileChanged events.
    let agent_for: HashMap<PathBuf, &'static str> =
        roots.iter().map(|r| (r.path.clone(), r.agent)).collect();

    let parsers: HashMap<&'static str, Arc<dyn Parser>> = {
        let mut m: HashMap<&'static str, Arc<dyn Parser>> = HashMap::new();
        m.insert("claude-code", Arc::new(parsers::claude::ClaudeParser));
        m.insert("codex", Arc::new(parsers::codex::CodexParser));
        m
    };

    let rescan = Duration::from_secs(60);
    let rx = watcher::spawn_real(root_paths.clone(), rescan);

    spawn_loop(rx, root_paths, agent_for, parsers)
}

/// Spawn the dispatch loop with a pre-built event channel. Used by
/// production (`spawn`) and integration tests (which inject a fake
/// receiver).
pub fn spawn_loop(
    mut rx: tokio::sync::mpsc::Receiver<watcher::WatcherEvent>,
    roots: Vec<PathBuf>,
    agent_for: HashMap<PathBuf, &'static str>,
    parsers: HashMap<&'static str, Arc<dyn Parser>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                watcher::WatcherEvent::FileChanged(path) => {
                    handle_change(&path, &roots, &agent_for, &parsers).await;
                }
                watcher::WatcherEvent::RescanTick => {
                    handle_rescan(&roots, &agent_for, &parsers).await;
                }
            }
        }
    })
}

async fn handle_change(
    path: &Path,
    roots: &[PathBuf],
    agent_for: &HashMap<PathBuf, &'static str>,
    parsers: &HashMap<&'static str, Arc<dyn Parser>>,
) {
    let agent = match classify(path, roots, agent_for) {
        Some(a) => a,
        None => return,
    };
    let parser = match parsers.get(agent) {
        Some(p) => p.clone(),
        None => return,
    };
    let path_str = path.to_string_lossy().to_string();

    // 1. Read state.
    let state = match tokio::task::spawn_blocking({
        let p = path_str.clone();
        let a = agent.to_string();
        move || {
            let c = crate::events::store::conn().lock().unwrap();
            state::get_or_fresh(&c, &p, &a)
        }
    }).await.expect("state task panicked") {
        Ok(s) => s,
        Err(e) => {
            crate::log::log("scraper", &format!("state read failed: {}", e));
            return;
        }
    };

    // 2. Compute current fingerprint, decide whether to reset offset.
    let cur_fp = match fingerprint(path) { Some(f) => f, None => return };
    let from_offset = if state.fingerprint == cur_fp { state.last_offset } else { 0 };

    // 3. Parse.
    let path_owned = path.to_path_buf();
    let parser_clone = parser.clone();
    let parse_result = tokio::task::spawn_blocking(move || {
        parser_clone.parse_from(&path_owned, from_offset)
    }).await.expect("parser task panicked");
    let result = match parse_result {
        Ok(r) => r,
        Err(e) => {
            crate::log::log("scraper", &format!("parse failed for {}: {}", path_str, e));
            return;
        }
    };

    // 4. Emit each record. Errors are logged and skipped; state advances
    //    only after a successful submit so retries land on the same offset.
    for rec in &result.records {
        let env = emit::build_envelope(rec, agent);
        let env_clone = env.clone();
        let submit_result = tokio::task::spawn_blocking(move || {
            emit::submit_envelope(&env_clone)
        }).await.expect("submit task panicked");
        if let Err(e) = submit_result {
            crate::log::log("scraper", &format!("emit failed: {}", e));
            // Bail out of this file; next tick retries from the unchanged offset.
            return;
        }
    }

    // 5. Advance state.
    let new_state = state::FileState {
        path: path_str.clone(),
        agent: agent.to_string(),
        fingerprint: cur_fp,
        last_offset: result.last_complete_offset,
        last_emit_ts: now_ms(),
    };
    let _ = tokio::task::spawn_blocking(move || {
        let c = crate::events::store::conn().lock().unwrap();
        state::upsert(&c, &new_state)
    }).await;
}

async fn handle_rescan(
    roots: &[PathBuf],
    agent_for: &HashMap<PathBuf, &'static str>,
    parsers: &HashMap<&'static str, Arc<dyn Parser>>,
) {
    // Discover all transcript files under all roots.
    for root in roots {
        let agent = match agent_for.get(root) { Some(a) => *a, None => continue };
        for path in walk_jsonl(root) {
            // Synthesize a FileChanged-equivalent: same handler.
            handle_change(&path, roots, agent_for, parsers).await;
            // Suppress the unused-var warning via a no-op use of `agent`.
            let _ = agent;
        }
    }
}

fn classify<'a>(
    path: &Path,
    roots: &[PathBuf],
    agent_for: &'a HashMap<PathBuf, &'static str>,
) -> Option<&'static str> {
    for root in roots {
        if path.starts_with(root) {
            return agent_for.get(root).copied();
        }
    }
    None
}

fn fingerprint(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let size = m.len();
    let mtime_ms = m.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some(format!("{}:{}", size, mtime_ms))
}

fn walk_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() { return out; }
    fn rec(p: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(p) { Ok(e) => e, Err(_) => return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() { rec(&path, out); }
            else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
    }
    rec(root, &mut out);
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scraper::watcher::testing::fake_pair;

    /// Verify that a FileChanged event for a freshly-written Claude
    /// fixture causes the dispatch loop to call submit_envelope, which
    /// inserts a row into the events store.
    #[tokio::test]
    async fn fake_watcher_drives_full_pipeline() {
        // Init the store with migrations applied (matches the daemon
        // startup sequence so emit::submit_envelope finds a real db).
        crate::events::store::init_for_tests();

        // Stage a tempdir + copy of a Claude fixture under it.
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = tmp.path().join("project1");
        std::fs::create_dir_all(&session_dir).unwrap();
        let session_path = session_dir.join("sess.jsonl");
        let fixture_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scraper/claude/turn_complete.jsonl")
        ).unwrap();
        std::fs::write(&session_path, &fixture_bytes).unwrap();

        // Wire up the loop with a fake watcher.
        let (tx, rx) = fake_pair();
        let roots = vec![tmp.path().to_path_buf()];
        let mut agent_for: HashMap<PathBuf, &'static str> = HashMap::new();
        agent_for.insert(tmp.path().to_path_buf(), "claude-code");
        let mut parsers: HashMap<&'static str, Arc<dyn Parser>> = HashMap::new();
        parsers.insert("claude-code", Arc::new(parsers::claude::ClaudeParser));
        let _h = spawn_loop(rx, roots, agent_for, parsers);

        // Fire the change.
        tx.send(watcher::WatcherEvent::FileChanged(session_path.clone())).await.unwrap();

        // Give the loop time to run.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Assert at least one turn.metrics row is in the store.
        let count: i64 = {
            let c = crate::events::store::conn().lock().unwrap();
            c.query_row(
                "SELECT COUNT(*) FROM events WHERE event_type = 'turn.metrics' AND source = 'agent-scraper'",
                [],
                |row| row.get(0),
            ).unwrap()
        };
        assert!(count >= 1, "expected at least one turn.metrics row, got {}", count);
    }
}
