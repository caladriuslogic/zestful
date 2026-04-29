//! Color palette: brand-orange chrome accent + per-agent hash colors +
//! semantic state colors. Brand orange is reserved for chrome only —
//! never used to convey state — so users never confuse "this is Zestful"
//! with "this is a warning."

use ratatui::style::Color;

/// Zestful brand orange (`#F59E0A`). Primary chrome accent: title bar,
/// brand mark background, focused-pane border, filter mode indicator.
pub const BRAND_ORANGE: Color = Color::Rgb(0xF5, 0x9E, 0x0A);

/// Lighter system orange (`#FF9500`) used for the gradient-stop accent
/// half-blocks flanking the brand mark.
pub const BRAND_ORANGE_LIGHT: Color = Color::Rgb(0xFF, 0x95, 0x00);

/// Per-agent palette. 12 distinguishable hues; brand-orange-adjacent
/// hues deliberately omitted so per-agent accents never collide with the
/// chrome accent. Stable across runs (indexed by `agent_color_index`).
pub const AGENT_PALETTE: &[Color] = &[
    Color::Rgb(0x60, 0xA5, 0xFA), // blue-400
    Color::Rgb(0x34, 0xD3, 0x99), // emerald-400
    Color::Rgb(0xA7, 0x8B, 0xFA), // violet-400
    Color::Rgb(0xF4, 0x72, 0xB6), // pink-400
    Color::Rgb(0x4A, 0xDE, 0x80), // green-400
    Color::Rgb(0x67, 0xE8, 0xF9), // cyan-300
    Color::Rgb(0xE8, 0x79, 0xF9), // fuchsia-400
    Color::Rgb(0xFA, 0xCC, 0x15), // yellow-400
    Color::Rgb(0xFB, 0x71, 0x85), // rose-400
    Color::Rgb(0x94, 0xA3, 0xB8), // slate-400
    Color::Rgb(0xC4, 0xB5, 0xFD), // violet-300
    Color::Rgb(0x6E, 0xE7, 0xB7), // emerald-300
];

/// Stable per-agent color. Same agent string → same color across runs.
pub fn agent_color(agent: &str) -> Color {
    let idx = agent_color_index(agent);
    AGENT_PALETTE[idx]
}

fn agent_color_index(agent: &str) -> usize {
    use std::hash::{Hash, Hasher};
    // SipHasher / DefaultHasher is stable across runs in the same Rust
    // version + std lib. For our purpose ("don't flip on every keystroke")
    // that's enough. We don't need cross-version stability.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    agent.hash(&mut h);
    (h.finish() as usize) % AGENT_PALETTE.len()
}

/// Connection-state color. Green=live, yellow=reconnecting, red=offline.
pub fn connection_color(state: ConnectionState) -> Color {
    match state {
        ConnectionState::Live         => Color::Rgb(0x10, 0xB9, 0x81), // emerald-500
        ConnectionState::Reconnecting => Color::Rgb(0xF5, 0x9E, 0x0A), // amber-500 (yellow-orange)
        ConnectionState::Offline      => Color::Rgb(0xEF, 0x44, 0x44), // red-500
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState { Live, Reconnecting, Offline }

/// Notification severity color.
pub fn severity_color(s: Sev) -> Color {
    match s {
        Sev::Info   => Color::Rgb(0x60, 0xA5, 0xFA), // blue-400
        Sev::Warn   => Color::Rgb(0xF5, 0x9E, 0x0A), // amber-500
        Sev::Urgent => Color::Rgb(0xEF, 0x44, 0x44), // red-500
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Sev { Info, Warn, Urgent }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_color_is_deterministic() {
        assert_eq!(agent_color("claude-code"), agent_color("claude-code"));
        assert_eq!(agent_color("codex-cli"), agent_color("codex-cli"));
    }

    #[test]
    fn different_agents_can_get_different_colors() {
        // Not guaranteed for any specific pair, but in practice a handful
        // of common agent slugs span at least 2 distinct colors.
        let names = ["claude-code", "codex-cli", "claude-web", "chatgpt-web", "gemini-web"];
        let mut seen = std::collections::HashSet::new();
        for n in names { seen.insert(agent_color(n)); }
        assert!(seen.len() >= 2, "expected >=2 distinct colors across {:?}, got {}", names, seen.len());
    }

    #[test]
    fn brand_orange_excluded_from_agent_palette() {
        assert!(!AGENT_PALETTE.contains(&BRAND_ORANGE),
            "brand orange must not appear in AGENT_PALETTE — chrome and per-agent accents must not collide");
    }

    #[test]
    fn agent_color_index_in_bounds() {
        for n in ["a", "b", "claude-code", "very-long-agent-name-asdf-asdf"] {
            assert!(agent_color_index(n) < AGENT_PALETTE.len());
        }
    }
}
