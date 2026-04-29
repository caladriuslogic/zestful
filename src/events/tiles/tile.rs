//! Tile struct + deterministic ID derivation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tile {
    /// Deterministic ID — "tile_<16 hex chars>" derived from identity tuple.
    pub id: String,
    /// Agent slug (claude-code, claude-web, vscode+codex, ...).
    pub agent: String,
    /// Strongest project signal — path or conversation slug.
    pub project_anchor: Option<String>,
    /// Human-display project name.
    pub project_label: Option<String>,
    /// "cli" | "browser" | "vscode".
    pub surface_kind: String,
    /// Raw surface token (used in identity).
    pub surface_token: String,
    /// Friendlier display: "tmux [zestful:0]", "VS Code window 1234", etc.
    pub surface_label: String,
    /// Oldest contributing event in window (unix ms).
    pub first_seen_at: i64,
    /// Newest contributing event in window (unix ms).
    pub last_seen_at: i64,
    /// Number of contributing events in window.
    pub event_count: i64,
    /// event_type of the most recent contributing event.
    pub latest_event_type: String,
    /// focus_uri from the most recent contributing event that had one.
    pub focus_uri: Option<String>,
}

/// Deterministic tile ID from the identity tuple. Same inputs always
/// produce the same ID — across reboots, daemon restarts, schema changes.
///
/// Format: `tile_<16 hex chars>` where the hex is the first 16 chars of
/// SHA-256(agent || "\n" || project_anchor || "\n" || surface_token).
/// 64 bits of identity, ~1 collision per ~4 billion — fine for tile
/// counts that are tens, not millions.
pub fn id_for(agent: &str, project_anchor: &str, surface_token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(agent.as_bytes());
    h.update(b"\n");
    h.update(project_anchor.as_bytes());
    h.update(b"\n");
    h.update(surface_token.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(5 + 16);
    out.push_str("tile_");
    for b in &bytes[..8] {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_deterministic_across_calls() {
        let a = id_for("claude-code", "/x/Fubar", "tmux:zestful/pane:%0");
        let b = id_for("claude-code", "/x/Fubar", "tmux:zestful/pane:%0");
        assert_eq!(a, b);
    }

    #[test]
    fn id_format_is_tile_underscore_16_hex() {
        let id = id_for("claude-code", "/x/Fubar", "tmux:zestful/pane:%0");
        assert!(id.starts_with("tile_"), "id = {}", id);
        let hex_part = &id[5..];
        assert_eq!(hex_part.len(), 16, "hex part length = {}", hex_part.len());
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()),
                "hex_part contains non-hex char: {}", hex_part);
    }

    #[test]
    fn id_differs_when_agent_differs() {
        let a = id_for("claude-code",  "/x/Fubar", "tmux:zestful/pane:%0");
        let b = id_for("codex-cli",    "/x/Fubar", "tmux:zestful/pane:%0");
        assert_ne!(a, b);
    }

    #[test]
    fn id_differs_when_project_differs() {
        let a = id_for("claude-code", "/x/Fubar",  "tmux:zestful/pane:%0");
        let b = id_for("claude-code", "/x/Wibble", "tmux:zestful/pane:%0");
        assert_ne!(a, b);
    }

    #[test]
    fn id_differs_when_surface_differs() {
        let a = id_for("claude-code", "/x/Fubar", "tmux:zestful/pane:%0");
        let b = id_for("claude-code", "/x/Fubar", "tmux:zestful/pane:%1");
        assert_ne!(a, b);
    }

    #[test]
    fn id_handles_empty_components() {
        // Should not panic on empty strings.
        let id = id_for("", "", "");
        assert!(id.starts_with("tile_"));
    }
}
