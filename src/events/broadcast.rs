//! Process-wide broadcast channel for projection-changed frames.
//!
//! The daemon sends one frame onto this channel each time a `POST /events`
//! request successfully persists an envelope. The SSE `/stream` handler
//! subscribes to it and forwards frames to connected clients. Global
//! (OnceLock) so handlers don't need an extra axum state parameter —
//! mirrors the style of `crate::events::store::conn`.

use serde::Serialize;
use std::sync::OnceLock;
use tokio::sync::broadcast;

/// Frame shape emitted on every successful `POST /events`. Consumers
/// (day one: Mac app) treat any frame as "something changed, go refetch".
/// The fields are hints — they let future consumers be smarter about
/// which projection to refetch, but nothing breaks if they're ignored.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ProjectionChangedFrame {
    /// Event types that triggered this frame. Empty for synthetic frames
    /// (initial-connect, lag-catchup).
    pub source_event_types: Vec<String>,
    /// Unix ms when the frame was generated.
    pub ts: i64,
    /// Optional reason. "initial" or "catchup" for synthetic frames.
    /// Absent for frames caused by a real `POST /events`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

const CAPACITY: usize = 16;

static SENDER: OnceLock<broadcast::Sender<ProjectionChangedFrame>> = OnceLock::new();

/// Get (lazy-init) the process-wide broadcast sender.
pub fn sender() -> &'static broadcast::Sender<ProjectionChangedFrame> {
    SENDER.get_or_init(|| {
        let (tx, _) = broadcast::channel(CAPACITY);
        tx
    })
}

/// Convenience: send a frame. SendError is swallowed — zero-subscriber is
/// not an error.
pub fn send(frame: ProjectionChangedFrame) {
    let _ = sender().send(frame);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_with_no_subscribers_is_silently_ok() {
        // Don't assume other tests haven't subscribed. Just confirm sender()
        // works and send() doesn't panic.
        send(ProjectionChangedFrame {
            source_event_types: vec!["turn.completed".into()],
            ts: 1_000,
            reason: None,
        });
    }

    #[tokio::test]
    async fn subscriber_receives_frame() {
        let mut rx = sender().subscribe();
        let frame = ProjectionChangedFrame {
            source_event_types: vec!["test".into()],
            ts: 2_000,
            reason: None,
        };
        send(frame.clone());
        // Drain any earlier frames from concurrent tests; look for ours.
        // Use a short timeout so the test can't hang.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(2);
        loop {
            tokio::select! {
                r = rx.recv() => {
                    if let Ok(f) = r {
                        if f == frame { return; }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    panic!("did not receive frame within deadline");
                }
            }
        }
    }
}
