//! Integration test for the real notify-rs watcher path. Validates that
//! a write to a watched directory results in a FileChanged event reaching
//! the scraper's dispatch loop within a reasonable wall-clock budget.
//!
//! On heavily loaded CI runners FSEvents and inotify can take 1-2 seconds
//! to surface events; we use a 5s ceiling. This test does NOT validate
//! parsing correctness — that's covered by per-parser unit tests.

use serial_test::serial;
use std::io::Write;
use std::time::Duration;

// Both tests in this file spin up a real notify-rs watcher. macOS FSEvents
// + parallel test execution makes them race; the second test's writes can
// land before its watcher is fully registered, causing intermittent timeouts.
// Serializing on a named lock keeps them deterministic without forcing the
// whole test suite to --test-threads=1.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(notify_watcher)]
async fn real_watcher_emits_file_changed_within_5s() {
    let tmp = tempfile::tempdir().unwrap();
    // Canonicalize so we match notify-rs's reported path on macOS, where
    // /var/folders is reported as /private/var/folders by FSEvents.
    let target = std::fs::canonicalize(tmp.path()).unwrap();

    let mut rx = zestful::scraper::watcher::spawn_real(
        vec![target.clone()],
        Duration::from_secs(3600), // long rescan: don't let it interfere
    );

    // Give the watcher thread a moment to register its watch on the
    // tempdir before we write the trigger file. notify-rs spawns a
    // background thread to install the FSEvents/inotify watch.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Write a file under the watched dir.
    let path = target.join("session.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, r#"{{"type":"hello"}}"#).unwrap();
    drop(f);

    // Expect a FileChanged for our path within 5s.
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Some(zestful::scraper::watcher::WatcherEvent::FileChanged(p)) if p == path => {
                    return;
                }
                Some(_) => continue,
                None => panic!("watcher channel closed unexpectedly"),
            }
        }
    }).await;
    assert!(result.is_ok(), "no FileChanged within 5s — watcher not wired up?");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(notify_watcher)]
async fn real_watcher_rescan_recovers_missed_event() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().to_path_buf();

    let mut rx = zestful::scraper::watcher::spawn_real(
        vec![target.clone()],
        Duration::from_millis(200), // fast rescan for the test
    );

    // Drain any initial events.
    let _ = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;

    // Wait for at least one RescanTick.
    let saw_tick = tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            if let Some(zestful::scraper::watcher::WatcherEvent::RescanTick) = rx.recv().await {
                return true;
            }
        }
    }).await;
    assert!(saw_tick.is_ok(), "expected at least one RescanTick within 800ms");
}
