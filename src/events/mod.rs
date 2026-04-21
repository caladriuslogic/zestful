//! Event protocol emission for Zestful.
//!
//! Maps agent-hook stdin payloads into structured events and POSTs them to
//! the Rust daemon on `127.0.0.1:21548/events`. Best-effort: errors never
//! propagate to callers.

pub mod device;
pub mod envelope;
pub mod map;
pub mod payload;
pub mod preview;
pub mod send;

pub use device::device_id;
pub use envelope::{Context, Correlation, Envelope, Subapplication};
pub use map::map_hook_payload;
pub use payload::Payload;
pub use send::send_to_daemon;
