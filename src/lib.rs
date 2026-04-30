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
