//! Application state + Action dispatch + pure helpers (filter, sort).
//!
//! `AppState` is the single source of truth for one render frame. The
//! event loop calls `apply(action)` which mutates state and returns a
//! `Vec<SideEffect>` for the loop to execute (HTTP refetches, focus
//! POSTs). Splitting state mutation from I/O keeps state pure and
//! testable.

use crate::events::tiles::tile::Tile;
use crate::events::notifications::notification::Notification;
use crate::events::store::query::EventRow;
use crate::cmd::top::keys::{Action, InputMode};
use std::time::Instant;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    LastSeenDesc,
    EventCountDesc,
    AgentAsc,
    CtxPctDesc,
    SessionCostDesc,
}

impl SortMode {
    pub fn next(self) -> SortMode {
        match self {
            SortMode::LastSeenDesc    => SortMode::EventCountDesc,
            SortMode::EventCountDesc  => SortMode::AgentAsc,
            SortMode::AgentAsc        => SortMode::CtxPctDesc,
            SortMode::CtxPctDesc      => SortMode::SessionCostDesc,
            SortMode::SessionCostDesc => SortMode::LastSeenDesc,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Live,
    Reconnecting,
    Offline(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { TilesList, Detail }

#[derive(Debug, Clone, PartialEq)]
pub enum SideEffect {
    RefetchTiles,
    RefetchNotifications,
    RefetchEventsForSelected,
    PostFocus { terminal_uri: String, tile_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // variants consumed by ui.rs ctx-band rendering in Task 9
pub enum CtxBand { Healthy, Warming, Critical }

/// Map a context ratio (0.0–1.0+) to a band. Uses spec-defined cutoffs.
#[allow(dead_code)] // wired into TUI event loop + renderer in Tasks 10/12/13
pub fn ctx_band(pct: f64) -> CtxBand {
    if pct < 0.70 { CtxBand::Healthy }
    else if pct < 0.90 { CtxBand::Warming }
    else { CtxBand::Critical }
}

/// Return the band to toast about if `current` is an upward crossing
/// from `prev`. `None` means: don't emit a toast.
#[allow(dead_code)] // used by update_tiles_and_emit_toasts (wired in Task 10)
pub fn crossed_ctx_band(prev: Option<CtxBand>, current: CtxBand) -> Option<CtxBand> {
    let rank = |b: CtxBand| match b {
        CtxBand::Healthy => 0u8, CtxBand::Warming => 1, CtxBand::Critical => 2,
    };
    match (prev, current) {
        (None, CtxBand::Healthy) => None,
        (None, b) => Some(b),
        (Some(p), c) if rank(c) > rank(p) => Some(c),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct ToastEntry {
    pub msg: String,
    pub since: Instant,
    pub lifetime: Duration,
}

#[derive(Debug)]
pub struct AppState {
    pub tiles: Vec<Tile>,
    pub notifications: Vec<Notification>,
    pub recent_events: Vec<EventRow>,
    pub connection: Connection,
    pub selected: usize,
    pub focused_pane: Pane,
    pub filter: String,
    pub input_mode: InputMode,
    pub sort: SortMode,
    pub notif_only: bool,
    pub help_open: bool,
    pub toast: Option<ToastEntry>,
    #[allow(dead_code)] // populated by /summary fetch + rendered by stat band in Tasks 10/12
    pub summary: Option<crate::events::summary::Summary>,
    #[allow(dead_code)] // populated by update_tiles_and_emit_toasts (wired in Task 10)
    pub seen_ctx_band: std::collections::HashMap<String, CtxBand>,
    pub should_quit: bool,
    pub fullscreen: bool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            tiles: Vec::new(),
            notifications: Vec::new(),
            recent_events: Vec::new(),
            connection: Connection::Offline("starting".to_string()),
            selected: 0,
            focused_pane: Pane::TilesList,
            filter: String::new(),
            input_mode: InputMode::Normal,
            sort: SortMode::LastSeenDesc,
            notif_only: false,
            help_open: false,
            toast: None,
            summary: None,
            seen_ctx_band: std::collections::HashMap::new(),
            should_quit: false,
            fullscreen: false,
        }
    }

    /// Tiles after filter+sort+notif_only — what the UI actually shows.
    pub fn visible_tiles(&self) -> Vec<&Tile> {
        let notif_tile_ids: std::collections::HashSet<&str> =
            self.notifications.iter().map(|n| n.tile_id.as_str()).collect();
        let mut out: Vec<&Tile> = self.tiles.iter()
            .filter(|t| !self.notif_only || notif_tile_ids.contains(t.id.as_str()))
            .filter(|t| matches_filter(&self.filter, t))
            .collect();
        sort_tiles(&mut out, self.sort);
        out
    }

    /// Currently-selected tile after applying visible_tiles ordering.
    pub fn selected_tile(&self) -> Option<&Tile> {
        self.visible_tiles().get(self.selected).copied()
    }

    /// Notifications attached to the currently-selected tile.
    pub fn notifications_for_selected(&self) -> Vec<&Notification> {
        let Some(t) = self.selected_tile() else { return Vec::new(); };
        self.notifications.iter().filter(|n| n.tile_id == t.id).collect()
    }

    /// Apply an action and return the side effects the loop should run.
    pub fn apply(&mut self, action: Action) -> Vec<SideEffect> {
        let mut fx = Vec::new();
        match action {
            Action::Quit => { self.should_quit = true; }

            Action::SelectNext => {
                let len = self.visible_tiles().len();
                if len > 0 && self.selected + 1 < len {
                    self.selected += 1;
                    self.recent_events.clear();
                    fx.push(SideEffect::RefetchEventsForSelected);
                }
            }
            Action::SelectPrev => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.recent_events.clear();
                    fx.push(SideEffect::RefetchEventsForSelected);
                }
            }
            Action::SelectFirst => {
                if self.selected != 0 {
                    self.selected = 0;
                    self.recent_events.clear();
                    fx.push(SideEffect::RefetchEventsForSelected);
                }
            }
            Action::SelectLast => {
                let len = self.visible_tiles().len();
                if len > 0 {
                    let last = len - 1;
                    if self.selected != last {
                        self.selected = last;
                        self.recent_events.clear();
                        fx.push(SideEffect::RefetchEventsForSelected);
                    }
                }
            }
            Action::PageDown => {
                let len = self.visible_tiles().len();
                if len > 0 {
                    let new = (self.selected + 10).min(len - 1);
                    if new != self.selected {
                        self.selected = new;
                        self.recent_events.clear();
                        fx.push(SideEffect::RefetchEventsForSelected);
                    }
                }
            }
            Action::PageUp => {
                let new = self.selected.saturating_sub(10);
                if new != self.selected {
                    self.selected = new;
                    self.recent_events.clear();
                    fx.push(SideEffect::RefetchEventsForSelected);
                }
            }
            // Two-pane layout: forward and backward both toggle. If a third
            // pane is added later, split the arms.
            Action::FocusNextPane | Action::FocusPrevPane => {
                self.focused_pane = match self.focused_pane {
                    Pane::TilesList => Pane::Detail,
                    Pane::Detail    => Pane::TilesList,
                };
            }

            Action::EnterFilterMode => { self.input_mode = InputMode::Filter; }
            Action::CommitFilter    => {
                // Keep the filter; return to Normal mode so navigation keys work.
                self.input_mode = InputMode::Normal;
            }
            Action::ExitFilterMode  => {
                // Cancel: priority is help → filter → no-op.
                if self.help_open {
                    self.help_open = false;
                } else {
                    self.input_mode = InputMode::Normal;
                    self.filter.clear();
                    self.selected = 0;
                }
            }
            Action::FilterChar(c)   => {
                self.filter.push(c);
                self.selected = 0;
            }
            Action::FilterBackspace => {
                self.filter.pop();
                self.selected = 0;
            }

            Action::Refresh => {
                fx.push(SideEffect::RefetchTiles);
                fx.push(SideEffect::RefetchNotifications);
                fx.push(SideEffect::RefetchEventsForSelected);
            }
            Action::Focus => {
                if let Some(t) = self.selected_tile() {
                    match &t.focus_uri {
                        Some(uri) => fx.push(SideEffect::PostFocus {
                            terminal_uri: uri.clone(),
                            tile_id: t.id.clone(),
                        }),
                        None => self.toast = Some(ToastEntry {
                            msg: "no focus URI for this tile (yet)".to_string(),
                            since: Instant::now(),
                            lifetime: Duration::from_secs(3),
                        }),
                    }
                }
            }

            Action::CycleSort       => { self.sort = self.sort.next(); self.selected = 0; }
            Action::ToggleNotifOnly => { self.notif_only = !self.notif_only; self.selected = 0; }
            Action::ToggleHelp      => { self.help_open = !self.help_open; }
        }
        fx
    }

    /// Replace `tiles` with `new_tiles`, scan for context-band crossings on
    /// each session, and emit at most one toast per call (most recent
    /// upward crossing wins). Sessions absent from `new_tiles` are evicted
    /// from `seen_ctx_band` to bound memory.
    #[allow(dead_code)] // wired into TUI event loop in Task 10
    pub fn update_tiles_and_emit_toasts(&mut self, new_tiles: Vec<Tile>) {
        let now = Instant::now();
        let mut latest_toast: Option<ToastEntry> = None;
        let mut active_sessions = std::collections::HashSet::<String>::new();
        for t in &new_tiles {
            let Some(m) = t.metrics.as_ref() else { continue; };
            let Some(pct) = m.context_pct else { continue; };
            active_sessions.insert(m.session_id.clone());
            let band = ctx_band(pct);
            let prev = self.seen_ctx_band.get(&m.session_id).copied();
            if let Some(emit_band) = crossed_ctx_band(prev, band) {
                let (label, lifetime) = match emit_band {
                    CtxBand::Warming  => ("70% context — keep an eye", Duration::from_secs(4)),
                    CtxBand::Critical => ("90% context — wrap up soon", Duration::from_secs(6)),
                    CtxBand::Healthy  => continue, // unreachable per crossed_ctx_band
                };
                let project = t.project_label.as_deref().unwrap_or("?");
                let msg = format!("{} @ {}: {}", t.agent, project, label);
                latest_toast = Some(ToastEntry { msg, since: now, lifetime });
            }
            self.seen_ctx_band.insert(m.session_id.clone(), band);
        }
        if let Some(toast) = latest_toast {
            self.toast = Some(toast);
        }
        self.seen_ctx_band.retain(|sid, _| active_sessions.contains(sid));
        self.tiles = new_tiles;
    }
}

/// Case-insensitive substring match against agent + project_label + surface_label.
fn matches_filter(needle: &str, t: &Tile) -> bool {
    if needle.is_empty() { return true; }
    let n = needle.to_lowercase();
    let in_field = |s: &str| s.to_lowercase().contains(&n);
    in_field(&t.agent)
        || t.project_label.as_deref().map(in_field).unwrap_or(false)
        || in_field(&t.surface_label)
}

/// Sort tiles in place per `mode`. Stable: ties preserve input order.
/// Tiles with `metrics: None` sort last under metric-driven modes.
fn sort_tiles(tiles: &mut Vec<&Tile>, mode: SortMode) {
    match mode {
        SortMode::LastSeenDesc => tiles.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at)),
        SortMode::EventCountDesc => tiles.sort_by(|a, b| b.event_count.cmp(&a.event_count)),
        SortMode::AgentAsc => tiles.sort_by(|a, b| a.agent.cmp(&b.agent)),
        SortMode::CtxPctDesc => tiles.sort_by(|a, b| {
            let ka = a.metrics.as_ref().and_then(|m| m.context_pct);
            let kb = b.metrics.as_ref().and_then(|m| m.context_pct);
            match (ka, kb) {
                (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None)    => std::cmp::Ordering::Less,
                (None, Some(_))    => std::cmp::Ordering::Greater,
                (None, None)       => std::cmp::Ordering::Equal,
            }
        }),
        SortMode::SessionCostDesc => tiles.sort_by(|a, b| {
            let ka = a.metrics.as_ref().and_then(|m| m.session_cost_usd);
            let kb = b.metrics.as_ref().and_then(|m| m.session_cost_usd);
            match (ka, kb) {
                (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None)    => std::cmp::Ordering::Less,
                (None, Some(_))    => std::cmp::Ordering::Greater,
                (None, None)       => std::cmp::Ordering::Equal,
            }
        }),
    }
}

/// Bin events into 60 per-minute buckets ending at `now_ms`. Returns
/// counts; caller renders to sparkline glyphs.
pub fn sparkline_bins(events: &[EventRow], now_ms: i64) -> [u32; 60] {
    let mut bins = [0u32; 60];
    let window_start = now_ms - 60 * 60_000;
    for e in events {
        let ts = e.event_ts;
        if ts < window_start || ts > now_ms { continue; }
        let minute = ((now_ms - ts) / 60_000) as usize;
        let idx = 59usize.saturating_sub(minute);
        bins[idx] += 1;
    }
    bins
}

/// Render a 60-bin counts array into a sparkline string of 60 glyphs.
///
/// Per the spec: empty buckets and very-low-count buckets both render
/// as `▁`. This means a single event in a window dominated by an
/// 8-event burst is visually indistinguishable from no events — that's
/// the intended fidelity (sparkline is "shape of activity," not exact
/// counts). For exact counts, a tabular view is the right tool.
pub fn sparkline_glyphs(bins: &[u32; 60]) -> String {
    const GLYPHS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = *bins.iter().max().unwrap_or(&0);
    if max == 0 {
        return GLYPHS[0].to_string().repeat(60);
    }
    bins.iter().map(|&c| {
        if c == 0 { GLYPHS[0] }
        else {
            let idx = ((c as u64 * (GLYPHS.len() as u64 - 1)) / max as u64) as usize;
            GLYPHS[idx.min(GLYPHS.len() - 1)]
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(agent: &str, project: &str, surface: &str, last: i64, events: i64, focus: Option<&str>) -> Tile {
        Tile {
            id: format!("tile_{}_{}_{}", agent, project, surface),
            agent: agent.to_string(),
            project_anchor: Some(project.to_string()),
            project_label: Some(project.to_string()),
            surface_kind: "cli".to_string(),
            surface_token: surface.to_string(),
            surface_label: surface.to_string(),
            first_seen_at: 0,
            last_seen_at: last,
            event_count: events,
            latest_event_type: "turn.completed".to_string(),
            focus_uri: focus.map(String::from),
            metrics: None,
        }
    }

    fn fixture_state() -> AppState {
        let mut s = AppState::new();
        s.tiles = vec![
            tile("claude-code", "zestful",  "tmux:z/pane:%0", 100, 50, Some("workspace://x")),
            tile("codex-cli",   "shelldon", "tmux:s/pane:%0", 200, 30, Some("workspace://y")),
            tile("claude-web",  "Claude",   "claude.ai",      150, 10, None),
        ];
        s
    }

    #[test]
    fn select_next_advances_within_bounds_and_emits_refetch_event() {
        let mut s = fixture_state();
        let fx = s.apply(Action::SelectNext);
        assert_eq!(s.selected, 1);
        assert_eq!(fx, vec![SideEffect::RefetchEventsForSelected]);
    }

    #[test]
    fn select_next_at_end_is_noop() {
        let mut s = fixture_state();
        s.selected = 2;
        let fx = s.apply(Action::SelectNext);
        assert_eq!(s.selected, 2);
        assert!(fx.is_empty());
    }

    #[test]
    fn refresh_emits_three_refetches() {
        let mut s = fixture_state();
        let fx = s.apply(Action::Refresh);
        assert!(fx.contains(&SideEffect::RefetchTiles));
        assert!(fx.contains(&SideEffect::RefetchNotifications));
        assert!(fx.contains(&SideEffect::RefetchEventsForSelected));
    }

    #[test]
    fn focus_with_uri_emits_post_focus() {
        let mut s = fixture_state();
        let fx = s.apply(Action::Focus);
        // selected=0 default, sort=LastSeenDesc → highest last_seen first
        // → codex-cli (last=200) selected first. Has focus_uri = workspace://y.
        assert_eq!(fx.len(), 1);
        match &fx[0] {
            SideEffect::PostFocus { terminal_uri, .. } => assert_eq!(terminal_uri, "workspace://y"),
            other => panic!("unexpected fx: {:?}", other),
        }
    }

    #[test]
    fn focus_without_uri_sets_toast() {
        let mut s = fixture_state();
        // Find the index of claude-web (no focus_uri) under default sort:
        // last_seen desc → codex(200), claude-web(150), claude-code(100). claude-web is at idx 1.
        s.selected = 1;
        let fx = s.apply(Action::Focus);
        assert!(fx.is_empty());
        assert!(s.toast.is_some());
        assert!(s.toast.as_ref().unwrap().msg.contains("no focus URI"));
    }

    #[test]
    fn filter_char_resets_selection_and_filters_visible() {
        let mut s = fixture_state();
        s.apply(Action::EnterFilterMode);
        s.apply(Action::FilterChar('c'));
        s.apply(Action::FilterChar('o'));
        s.apply(Action::FilterChar('d'));
        assert_eq!(s.filter, "cod");
        let v = s.visible_tiles();
        assert!(v.iter().all(|t| t.agent.contains("cod") || t.project_label.as_deref().unwrap_or("").contains("cod") || t.surface_label.contains("cod")));
    }

    #[test]
    fn cycle_sort_walks_through_modes() {
        let mut s = fixture_state();
        assert_eq!(s.sort, SortMode::LastSeenDesc);
        s.apply(Action::CycleSort); assert_eq!(s.sort, SortMode::EventCountDesc);
        s.apply(Action::CycleSort); assert_eq!(s.sort, SortMode::AgentAsc);
        s.apply(Action::CycleSort); assert_eq!(s.sort, SortMode::CtxPctDesc);
        s.apply(Action::CycleSort); assert_eq!(s.sort, SortMode::SessionCostDesc);
        s.apply(Action::CycleSort); assert_eq!(s.sort, SortMode::LastSeenDesc);
    }

    #[test]
    fn toggle_notif_only_filters_to_notification_bearing_tiles() {
        let mut s = fixture_state();
        s.notifications = vec![Notification {
            id: "notif_x".to_string(),
            rule_id: "agent_completed".to_string(),
            tile_id: s.tiles[0].id.clone(),
            agent: s.tiles[0].agent.clone(),
            project_label: None,
            severity: crate::events::notifications::rule::Severity::Info,
            message: "done".to_string(),
            trigger_event_id: "ev_1".to_string(),
            triggered_at_ms: 100,
            focus_uri: None,
            push: false,
        }];
        s.apply(Action::ToggleNotifOnly);
        let v = s.visible_tiles();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, s.tiles[0].id);
    }

    #[test]
    fn quit_sets_should_quit() {
        let mut s = fixture_state();
        s.apply(Action::Quit);
        assert!(s.should_quit);
    }

    #[test]
    fn esc_closes_help_overlay_first_when_open() {
        let mut s = fixture_state();
        s.help_open = true;
        s.input_mode = InputMode::Filter;
        s.filter = "abc".to_string();
        s.apply(Action::ExitFilterMode);
        // Help closes, but filter and input_mode are preserved (help has priority).
        assert!(!s.help_open);
        assert_eq!(s.input_mode, InputMode::Filter);
        assert_eq!(s.filter, "abc");
    }

    #[test]
    fn commit_filter_keeps_query_and_returns_to_normal() {
        let mut s = fixture_state();
        s.input_mode = InputMode::Filter;
        s.filter = "cla".to_string();
        s.apply(Action::CommitFilter);
        assert_eq!(s.input_mode, InputMode::Normal);
        assert_eq!(s.filter, "cla");
    }

    #[test]
    fn sparkline_bins_distribute_events_into_correct_minute_slots() {
        let now = 60 * 60 * 1000; // 1 hour from epoch
        let make = |ts: i64| EventRow {
            id: 0, received_at: 0, event_id: "e".to_string(), event_type: "x".to_string(),
            source: "test".to_string(), session_id: None, project: None, host: "h".to_string(),
            os_user: "u".to_string(), device_id: "d".to_string(), event_ts: ts, seq: 0,
            source_pid: 0, schema_version: 1, correlation: None, context: None, payload: None,
        };
        // Three events: now (newest bin = 59), 30 min ago (bin 29), 59 min ago (bin 0).
        let events = vec![make(now), make(now - 30 * 60_000), make(now - 59 * 60_000)];
        let bins = sparkline_bins(&events, now);
        assert_eq!(bins[59], 1);
        assert_eq!(bins[29], 1);
        assert_eq!(bins[0], 1);
        // Empty buckets should be 0.
        assert_eq!(bins[1], 0);
    }

    #[test]
    fn sparkline_glyphs_renders_zero_array_as_min_glyph() {
        let bins = [0u32; 60];
        let s = sparkline_glyphs(&bins);
        assert_eq!(s.chars().count(), 60);
        assert!(s.chars().all(|c| c == '▁'));
    }

    #[test]
    fn sparkline_glyphs_uses_full_block_for_max() {
        let mut bins = [0u32; 60];
        bins[10] = 8;
        bins[20] = 4;
        let s: Vec<char> = sparkline_glyphs(&bins).chars().collect();
        assert_eq!(s[10], '█'); // max
        assert!(s[20] != '▁' && s[20] != '█'); // mid-range
    }

    #[test]
    fn sort_mode_cycles_through_five_states() {
        let mut s = SortMode::LastSeenDesc;
        s = s.next(); assert_eq!(s, SortMode::EventCountDesc);
        s = s.next(); assert_eq!(s, SortMode::AgentAsc);
        s = s.next(); assert_eq!(s, SortMode::CtxPctDesc);
        s = s.next(); assert_eq!(s, SortMode::SessionCostDesc);
        s = s.next(); assert_eq!(s, SortMode::LastSeenDesc);
    }

    #[test]
    fn ctx_band_thresholds() {
        assert_eq!(ctx_band(0.0),  CtxBand::Healthy);
        assert_eq!(ctx_band(0.69), CtxBand::Healthy);
        assert_eq!(ctx_band(0.70), CtxBand::Warming);
        assert_eq!(ctx_band(0.89), CtxBand::Warming);
        assert_eq!(ctx_band(0.90), CtxBand::Critical);
        assert_eq!(ctx_band(1.50), CtxBand::Critical);
    }

    #[test]
    fn crossed_ctx_band_only_emits_on_upward_crossing() {
        // None → any band emits the new band (first sighting), except Healthy.
        assert_eq!(crossed_ctx_band(None, CtxBand::Healthy),  None);
        assert_eq!(crossed_ctx_band(None, CtxBand::Warming),  Some(CtxBand::Warming));
        assert_eq!(crossed_ctx_band(None, CtxBand::Critical), Some(CtxBand::Critical));
        // Upward crossings emit.
        assert_eq!(crossed_ctx_band(Some(CtxBand::Healthy), CtxBand::Warming),
                   Some(CtxBand::Warming));
        assert_eq!(crossed_ctx_band(Some(CtxBand::Warming), CtxBand::Critical),
                   Some(CtxBand::Critical));
        assert_eq!(crossed_ctx_band(Some(CtxBand::Healthy), CtxBand::Critical),
                   Some(CtxBand::Critical));
        // Same band — no emit.
        assert_eq!(crossed_ctx_band(Some(CtxBand::Warming),  CtxBand::Warming),  None);
        assert_eq!(crossed_ctx_band(Some(CtxBand::Critical), CtxBand::Critical), None);
        // Downward — no emit.
        assert_eq!(crossed_ctx_band(Some(CtxBand::Critical), CtxBand::Warming),  None);
        assert_eq!(crossed_ctx_band(Some(CtxBand::Critical), CtxBand::Healthy),  None);
        assert_eq!(crossed_ctx_band(Some(CtxBand::Warming),  CtxBand::Healthy),  None);
    }

    #[test]
    fn update_tiles_emits_toast_on_first_warming_crossing() {
        use crate::events::tiles::tile::{Tile, TileMetrics};
        use crate::events::payload::TurnTokens;

        let mut s = AppState::new();
        let tile = Tile {
            id: "tile_a".into(), agent: "claude-code".into(),
            project_anchor: Some("/x".into()), project_label: Some("x".into()),
            surface_kind: "cli".into(), surface_token: "t".into(),
            surface_label: "t".into(), first_seen_at: 0, last_seen_at: 0,
            event_count: 0, latest_event_type: "x".into(), focus_uri: None,
            metrics: Some(TileMetrics {
                model: "claude-3-5-sonnet-20241022".into(),
                session_id: "s1".into(),
                context_pct: Some(0.75),
                context_used_tokens: 150_000,
                context_max_tokens: Some(200_000),
                session_cost_usd: Some(0.10),
                cache_hit_pct: Some(0.5),
                burn_rate_usd_hr: Some(0.5),
                tokens: TurnTokens::default(),
            }),
        };
        s.update_tiles_and_emit_toasts(vec![tile.clone()]);
        // First sighting at 75% (Warming) emits.
        assert!(s.toast.is_some(), "expected a toast to fire");
        let msg = &s.toast.as_ref().unwrap().msg;
        assert!(msg.contains("70%"), "expected '70%' in toast: {}", msg);

        // Refresh with the same band — no new toast.
        s.toast = None;
        s.update_tiles_and_emit_toasts(vec![tile.clone()]);
        assert!(s.toast.is_none(), "no re-toast for same band");

        // Crossing into Critical — new toast.
        let mut tile2 = tile.clone();
        tile2.metrics.as_mut().unwrap().context_pct = Some(0.92);
        s.update_tiles_and_emit_toasts(vec![tile2]);
        let msg2 = &s.toast.as_ref().unwrap().msg;
        assert!(msg2.contains("90%"), "expected '90%' in critical toast: {}", msg2);
    }

    #[test]
    fn sort_ctx_pct_desc_places_none_at_bottom() {
        use crate::events::tiles::tile::{Tile, TileMetrics};
        use crate::events::payload::TurnTokens;
        let mk = |id: &str, agent: &str, pct: Option<f64>| Tile {
            id: id.into(), agent: agent.into(),
            project_anchor: Some("/x".into()), project_label: Some("x".into()),
            surface_kind: "cli".into(), surface_token: "t".into(),
            surface_label: "t".into(), first_seen_at: 0, last_seen_at: 0,
            event_count: 0, latest_event_type: "x".into(), focus_uri: None,
            metrics: pct.map(|p| TileMetrics {
                model: "m".into(), session_id: "s".into(),
                context_pct: Some(p),
                context_used_tokens: 0,
                context_max_tokens: None,
                session_cost_usd: None,
                cache_hit_pct: None, burn_rate_usd_hr: None,
                tokens: TurnTokens::default(),
            }),
        };
        let lo   = mk("lo",   "a", Some(0.10));
        let hi   = mk("hi",   "a", Some(0.90));
        let none = mk("none", "a", None);
        let mut tiles: Vec<&Tile> = vec![&lo, &hi, &none];
        sort_tiles(&mut tiles, SortMode::CtxPctDesc);
        assert_eq!(tiles[0].id, "hi");
        assert_eq!(tiles[1].id, "lo");
        assert_eq!(tiles[2].id, "none");
    }
}
