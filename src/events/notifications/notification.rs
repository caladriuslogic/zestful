//! Notification struct + id_for helper.

use crate::events::notifications::rule::Severity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Deterministic ID — "notif_<16 hex chars>" derived from (rule_id, tile_id).
    pub id: String,
    pub rule_id: String,
    pub tile_id: String,
    pub agent: String,
    pub project_label: Option<String>,
    pub severity: Severity,
    pub message: String,
    pub trigger_event_id: String,
    pub triggered_at_ms: i64,
    pub focus_uri: Option<String>,
}

/// Deterministic notification ID from (rule_id, tile_id). Mirrors
/// `crate::events::tiles::tile::id_for`: `notif_` prefix, 16-hex
/// first 8 bytes of SHA-256, newline separator.
pub fn id_for(rule_id: &str, tile_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(rule_id.as_bytes());
    h.update(b"\n");
    h.update(tile_id.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(6 + 16);
    out.push_str("notif_");
    for b in &bytes[..8] {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_deterministic() {
        let a = id_for("agent_completed", "tile_abc1234567890def");
        let b = id_for("agent_completed", "tile_abc1234567890def");
        assert_eq!(a, b);
    }

    #[test]
    fn id_format_is_notif_underscore_16_hex() {
        let id = id_for("agent_completed", "tile_abc1234567890def");
        assert!(id.starts_with("notif_"), "id = {}", id);
        let hex = &id[6..];
        assert_eq!(hex.len(), 16);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn id_differs_when_rule_differs() {
        let a = id_for("agent_completed", "tile_abc");
        let b = id_for("agent_notified", "tile_abc");
        assert_ne!(a, b);
    }

    #[test]
    fn id_differs_when_tile_differs() {
        let a = id_for("agent_completed", "tile_abc");
        let b = id_for("agent_completed", "tile_def");
        assert_ne!(a, b);
    }
}
