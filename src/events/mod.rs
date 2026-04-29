//! Event protocol emission for Zestful.
//!
//! Maps agent-hook stdin payloads into structured events and POSTs them to
//! the Rust daemon on `127.0.0.1:21548/events`. Best-effort: errors never
//! propagate to callers.

pub mod backend_forwarder;
pub mod broadcast;
pub mod device;
pub mod env_capture;
pub mod envelope;
pub mod map;
pub mod payload;
pub mod preview;
pub mod send;
pub mod severity;
pub mod notifications;
pub mod store;
pub mod tiles;

pub use map::{map_cli_notify, map_hook_payload, map_watch_completed};
pub use send::send_to_daemon;
