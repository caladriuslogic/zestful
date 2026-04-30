//! Library exports. The `zestful` crate is primarily a binary; this lib
//! exists so integration tests in `tests/*.rs` can reach internal modules
//! via `zestful::...` paths. The binary's `main.rs` re-uses the same modules.

pub mod cmd;
pub mod config;
pub mod events;
pub mod hooks;
pub mod log;
pub mod scraper;
pub mod workspace;

/// Test-only entrypoint — exposes the production watcher to integration
/// tests in `tests/scraper_watcher_real.rs`. Not for production use.
pub fn scraper_watcher_spawn_real_for_tests(
    roots: Vec<std::path::PathBuf>,
    rescan: std::time::Duration,
) -> tokio::sync::mpsc::Receiver<scraper::watcher::WatcherEvent> {
    scraper::watcher::spawn_real(roots, rescan)
}
