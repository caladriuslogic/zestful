//! Cross-platform file watcher abstraction. Production uses notify-rs;
//! tests inject a `FakeWatcherSource` that drives the dispatch loop
//! synchronously without touching real OS APIs.
//!
//! Two-source design: live OS events go through one channel, periodic
//! rescan ticks go through another. Both feed the same downstream
//! dispatch loop; the source distinction matters only for telemetry.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum WatcherEvent {
    /// A live notify-rs event for `path`.
    FileChanged(PathBuf),
    /// A periodic rescan tick. Dispatch loop walks state vs. disk and
    /// synthesizes FileChanged for each drift.
    RescanTick,
}

/// Production watcher: registers recursive notify watchers on each root
/// and forwards modify events. Debounces per-path with a 100ms window.
pub fn spawn_real(
    roots: Vec<PathBuf>,
    rescan_interval: Duration,
) -> mpsc::Receiver<WatcherEvent> {
    let (tx, rx) = mpsc::channel(1024);

    // Periodic rescan tick.
    let tx_tick = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(rescan_interval);
        loop {
            interval.tick().await;
            if tx_tick.send(WatcherEvent::RescanTick).await.is_err() {
                break;
            }
        }
    });

    // Real notify-rs watcher.
    let tx_notify = tx;
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        let (sync_tx, sync_rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = sync_tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("scraper: watcher init failed: {}", e);
                return;
            }
        };

        for root in &roots {
            // Deferred-watch fallback: if the root doesn't exist yet,
            // walk up to the nearest existing ancestor and register
            // there. The dispatch loop's path filter ignores anything
            // outside the configured root set.
            let target = nearest_existing_ancestor(root)
                .unwrap_or_else(|| PathBuf::from("/"));
            if let Err(e) = watcher.watch(&target, RecursiveMode::Recursive) {
                eprintln!("scraper: watch register failed for {:?}: {}", target, e);
            }
        }

        // Per-path debounce window.
        let mut last_emit: std::collections::HashMap<PathBuf, std::time::Instant>
            = std::collections::HashMap::new();
        let debounce = Duration::from_millis(100);

        for res in sync_rx {
            let event = match res {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("scraper: watcher error: {}", e);
                    continue;
                }
            };
            for path in event.paths {
                let now = std::time::Instant::now();
                if let Some(prev) = last_emit.get(&path) {
                    if now.duration_since(*prev) < debounce {
                        continue;
                    }
                }
                last_emit.insert(path.clone(), now);
                if tx_notify.blocking_send(WatcherEvent::FileChanged(path)).is_err() {
                    return;
                }
            }
        }
    });

    rx
}

fn nearest_existing_ancestor(p: &Path) -> Option<PathBuf> {
    let mut cur = p.to_path_buf();
    loop {
        if cur.exists() { return Some(cur); }
        if !cur.pop() { return None; }
    }
}

#[cfg(test)]
pub mod testing {
    //! Test-only: fake watcher source lets integration tests drive the
    //! dispatch loop deterministically.
    use super::*;
    pub fn fake_pair() -> (mpsc::Sender<WatcherEvent>, mpsc::Receiver<WatcherEvent>) {
        mpsc::channel(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_existing_ancestor_returns_self_if_exists() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            nearest_existing_ancestor(tmp.path()),
            Some(tmp.path().to_path_buf())
        );
    }

    #[test]
    fn nearest_existing_ancestor_walks_up() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("a/b/c");
        assert_eq!(
            nearest_existing_ancestor(&nonexistent),
            Some(tmp.path().to_path_buf())
        );
    }

    #[test]
    fn nearest_existing_ancestor_returns_none_for_unrooted() {
        // A purely relative path with no existing component walks up to ""
        // then pops to nothing, returning None. The watcher's blanket
        // /-fallback handles the None case.
        assert!(nearest_existing_ancestor(Path::new("totally-fake-segment")).is_none());
    }
}
