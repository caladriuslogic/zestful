//! `Severity` is the wire-level severity classification for notifications.
//!
//! Used by both the event payload (as an emitter intent hint, e.g.
//! `AgentNotified.severity_hint`) and by notification rules (as the
//! authoritative per-firing severity on `NotificationBody`). Wire format
//! is `info | warn | urgent` (lowercase).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Urgent,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Urgent => "urgent",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_display_is_lowercase() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warn.to_string(), "warn");
        assert_eq!(Severity::Urgent.to_string(), "urgent");
    }

    #[test]
    fn severity_serializes_lowercase() {
        let s = serde_json::to_string(&Severity::Warn).unwrap();
        assert_eq!(s, "\"warn\"");
    }

    #[test]
    fn severity_deserializes_lowercase() {
        let s: Severity = serde_json::from_str("\"urgent\"").unwrap();
        assert_eq!(s, Severity::Urgent);
    }
}
