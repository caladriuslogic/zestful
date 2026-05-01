//! Tile struct + deterministic ID derivation.

use crate::events::payload::TurnTokens;
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
    /// Per-tile metrics from the active session, when available.
    /// `None` until at least one `turn.metrics` event has been seen for
    /// this tile's `session_id`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metrics: Option<TileMetrics>,
}

/// Per-tile metrics, attached server-side by the tiles projection.
/// "Active session" = the session_id of the most recent turn.metrics
/// whose tile-key matches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TileMetrics {
    pub model: String,
    pub session_id: String,
    /// 0.0–1.0 — `payload.context.ratio` of the latest turn.metrics.
    /// `None` for unknown models (no max_tokens).
    pub context_pct: Option<f64>,
    /// `payload.context.used_tokens` of the latest turn.metrics —
    /// drives the "730K / 1M tokens" detail-pane display.
    pub context_used_tokens: u64,
    /// `payload.context.max_tokens` of the latest turn.metrics.
    /// `None` for unknown models.
    pub context_max_tokens: Option<u64>,
    /// Σ cost_estimate_usd for all turn.metrics on the active session.
    pub session_cost_usd: Option<f64>,
    /// Σ cache_read / (Σ input + Σ cache_read) for the active session.
    /// `None` if denominator is 0.
    pub cache_hit_pct: Option<f64>,
    /// Cost in last 60 min on the session, normalised to 1 hour.
    pub burn_rate_usd_hr: Option<f64>,
    /// Σ each token field for the active session.
    pub tokens: TurnTokens,
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

    #[test]
    fn tile_metrics_roundtrip_full() {
        use crate::events::payload::TurnTokens;
        let m = TileMetrics {
            model: "claude-opus-4-7".to_string(),
            session_id: "sess_abc".to_string(),
            context_pct: Some(0.73),
            context_used_tokens: 730_000,
            context_max_tokens: Some(1_000_000),
            session_cost_usd: Some(0.42),
            cache_hit_pct: Some(0.71),
            burn_rate_usd_hr: Some(1.15),
            tokens: TurnTokens {
                input: 12450, output: 832, cache_read: 8120,
                cache_write: 0, reasoning: 0,
            },
        };
        let j = serde_json::to_string(&m).unwrap();
        let back: TileMetrics = serde_json::from_str(&j).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn tile_metrics_roundtrip_with_unknowns() {
        let m = TileMetrics {
            model: "unknown".to_string(),
            session_id: "sess_xyz".to_string(),
            context_pct: None,
            context_used_tokens: 0,
            context_max_tokens: None,
            session_cost_usd: None,
            cache_hit_pct: None,
            burn_rate_usd_hr: None,
            tokens: Default::default(),
        };
        let j = serde_json::to_string(&m).unwrap();
        let back: TileMetrics = serde_json::from_str(&j).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn tile_metrics_field_is_optional_on_tile() {
        // A wire payload missing `metrics` deserializes with metrics: None.
        let json = serde_json::json!({
            "id": "tile_abc",
            "agent": "x",
            "project_anchor": null,
            "project_label": null,
            "surface_kind": "cli",
            "surface_token": "t",
            "surface_label": "t",
            "first_seen_at": 0, "last_seen_at": 0,
            "event_count": 0, "latest_event_type": "x",
            "focus_uri": null
        });
        let t: Tile = serde_json::from_value(json).unwrap();
        assert!(t.metrics.is_none());
    }
}
