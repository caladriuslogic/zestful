//! ratatui rendering. Pure functions over `&AppState`.

use crate::cmd::top::app::{AppState, Connection, Pane};
use crate::cmd::top::colors::{self, BRAND_ORANGE, BRAND_ORANGE_LIGHT};
use crate::cmd::top::keys::InputMode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

pub fn draw(f: &mut Frame, state: &AppState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),    // header
            Constraint::Min(1),       // body
            Constraint::Length(1),    // status bar
        ])
        .split(area);

    draw_header(f, chunks[0], state);
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(chunks[1]);
    draw_tiles_list(f, body_chunks[0], state);
    draw_detail_pane(f, body_chunks[1], state);
    draw_status_bar(f, chunks[2], state);
}

pub fn draw_header(f: &mut Frame, area: Rect, _state: &AppState) {
    // Brand mark: ▌Z▐ where Z has orange bg + black bold fg, flanked by
    // gradient-stop accent half-blocks.
    let brand = Line::from(vec![
        Span::styled("▌", Style::default().fg(BRAND_ORANGE_LIGHT)),
        Span::styled("Z", Style::default().bg(BRAND_ORANGE).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::styled("▐", Style::default().fg(BRAND_ORANGE_LIGHT)),
        Span::raw("  "),
        Span::styled("zestful top", Style::default().add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(brand), area);
}

pub fn draw_status_bar(f: &mut Frame, area: Rect, state: &AppState) {
    // Left side: connection state + counts. Pull colors from the shared
    // `colors` module so chrome and per-state hues stay in sync.
    let (dot, label, color_state) = match &state.connection {
        Connection::Live         => ("●", "live",          colors::ConnectionState::Live),
        Connection::Reconnecting => ("◐", "reconnecting…", colors::ConnectionState::Reconnecting),
        Connection::Offline(_)   => ("○", "offline",       colors::ConnectionState::Offline),
    };
    let dot_color = colors::connection_color(color_state);
    let counts = format!("{} tiles · {} notifs", state.tiles.len(), state.notifications.len());

    let left = Line::from(vec![
        Span::styled(dot, Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::raw(label),
        Span::raw("  "),
        Span::styled(counts, Style::default().fg(Color::Gray)),
    ]);

    // Right side: hint bar OR filter mode display.
    let right = if state.input_mode == InputMode::Filter {
        Line::from(vec![
            Span::styled("/", Style::default().fg(BRAND_ORANGE)),
            Span::styled(state.filter.clone(), Style::default().fg(BRAND_ORANGE)),
            Span::styled("_", Style::default().fg(BRAND_ORANGE).add_modifier(Modifier::SLOW_BLINK)),
        ])
    } else {
        Line::from(Span::styled(
            "↑↓ nav · Enter focus · / filter · ? help · q quit",
            Style::default().fg(Color::DarkGray),
        ))
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    f.render_widget(Paragraph::new(left), cols[0]);
    f.render_widget(Paragraph::new(right).alignment(ratatui::layout::Alignment::Right), cols[1]);
}

// Used by later tasks — placeholder so subsequent tasks compile.
pub fn draw_help_overlay(_f: &mut Frame, _state: &AppState) { /* Task 10 */ }
pub fn draw_empty_state(_f: &mut Frame, _area: Rect, _state: &AppState) { /* Task 9/10 */ }

pub fn draw_tiles_list(f: &mut Frame, area: Rect, state: &AppState) {
    use crate::cmd::top::colors::agent_color;
    let visible = state.visible_tiles();
    let now_ms = now_ms();

    // Border highlights when this pane is focused.
    let border_style = if state.focused_pane == Pane::TilesList {
        Style::default().fg(BRAND_ORANGE)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(" AGENTS ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if visible.is_empty() {
        let msg = if state.tiles.is_empty() {
            empty_message(state)
        } else {
            "No tiles match the current filter.".to_string()
        };
        f.render_widget(Paragraph::new(msg).style(Style::default().fg(Color::Gray)), inner);
        return;
    }

    // Notification tile-id set for the ⚠ glyph.
    let notif_ids: std::collections::HashSet<&str> =
        state.notifications.iter().map(|n| n.tile_id.as_str()).collect();

    let mut lines: Vec<Line> = Vec::with_capacity(visible.len());
    for (idx, t) in visible.iter().enumerate() {
        let cursor = if idx == state.selected { "▶ " } else { "  " };
        // Generic "has notifications" indicator — amber-400, NOT brand orange
        // (chrome-vs-state rule: brand orange is reserved for chrome only).
        let glyph = if notif_ids.contains(t.id.as_str()) {
            Span::styled(" ⚠", Style::default().fg(colors::severity_color(&crate::events::notifications::rule::Severity::Warn)))
        } else {
            Span::raw("  ")
        };
        let agent_style = Style::default().fg(agent_color(&t.agent)).add_modifier(Modifier::BOLD);
        let project = t.project_label.as_deref().unwrap_or("-");
        let last = relative_time(t.last_seen_at, now_ms);
        lines.push(Line::from(vec![
            Span::styled(cursor, Style::default().fg(agent_color(&t.agent))),
            Span::styled(format!("{:<14}", truncate(&t.agent, 14)), agent_style),
            Span::raw(" "),
            Span::raw(format!("{:<10}", truncate(project, 10))),
            Span::raw(" "),
            Span::styled(format!("{:>4}", last), Style::default().fg(Color::DarkGray)),
            glyph,
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn draw_detail_pane(f: &mut Frame, area: Rect, state: &AppState) {
    use crate::cmd::top::app::sparkline_glyphs;
    let border_style = if state.focused_pane == Pane::Detail {
        Style::default().fg(BRAND_ORANGE)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = state.selected_tile()
        .map(|t| format!(" {} · {} ", t.agent, t.project_label.as_deref().unwrap_or("-")))
        .unwrap_or_else(|| " — ".to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(t) = state.selected_tile() else {
        let msg = "No tile selected.";
        f.render_widget(Paragraph::new(msg).style(Style::default().fg(Color::Gray)), inner);
        return;
    };

    // Stack: surface_label · counts · sparkline · recent · notifications
    let now = now_ms();
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(t.surface_label.clone(), Style::default().fg(Color::Cyan))));
    lines.push(Line::from(format!(
        "{} events · first {} ago · last {} ago",
        t.event_count,
        relative_time(t.first_seen_at, now),
        relative_time(t.last_seen_at, now),
    )));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled("Activity (last hour)", Style::default().add_modifier(Modifier::BOLD))));
    let bins = crate::cmd::top::app::sparkline_bins(&state.recent_events, now);
    lines.push(Line::from(Span::styled(
        sparkline_glyphs(&bins),
        Style::default().fg(Color::Rgb(0x60, 0xA5, 0xFA)),
    )));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled("Recent", Style::default().add_modifier(Modifier::BOLD))));
    if state.recent_events.is_empty() {
        lines.push(Line::from(Span::styled("  (no events)", Style::default().fg(Color::DarkGray))));
    } else {
        for e in state.recent_events.iter().take(8) {
            let when = relative_time(e.event_ts, now);
            lines.push(Line::from(vec![
                Span::styled(format!("  {:>5}  ", when), Style::default().fg(Color::DarkGray)),
                Span::raw(e.event_type.clone()),
            ]));
        }
    }
    lines.push(Line::from(""));

    let notifs = state.notifications_for_selected();
    lines.push(Line::from(Span::styled("Notifications", Style::default().add_modifier(Modifier::BOLD))));
    if notifs.is_empty() {
        lines.push(Line::from(Span::styled("  (none)", Style::default().fg(Color::DarkGray))));
    } else {
        for n in notifs {
            let glyph = match n.severity {
                crate::events::notifications::rule::Severity::Info   => "·",
                crate::events::notifications::rule::Severity::Warn   => "⚠",
                crate::events::notifications::rule::Severity::Urgent => "!",
            };
            let when = relative_time(n.triggered_at_ms, now);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph, Style::default().fg(colors::severity_color(&n.severity))),
                Span::raw(format!("  {}  ", n.message)),
                Span::styled(when, Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Same algorithm as `cmd/tiles.rs:relative_time` — kept duplicated here
/// to avoid making `cmd::tiles::relative_time` pub. Trivial enough that
/// duplication is the right call.
fn relative_time(then_ms: i64, now_ms: i64) -> String {
    let delta = (now_ms - then_ms).max(0);
    if delta < 60_000     { return format!("{}s",  delta / 1000); }
    if delta < 3_600_000  { return format!("{}m",  delta / 60_000); }
    if delta < 86_400_000 { return format!("{}h",  delta / 3_600_000); }
    format!("{}d", delta / 86_400_000)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { return s.to_string(); }
    let cut: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{}…", cut)
}

fn empty_message(state: &AppState) -> String {
    match &state.connection {
        Connection::Offline(reason) => format!("Daemon not reachable: {}.\nPress r to retry.", reason),
        Connection::Reconnecting    => "Reconnecting to daemon…".to_string(),
        Connection::Live            => "No agent activity in the last 24h.\nListening for new events…".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::top::app::AppState;
    use ratatui::{backend::TestBackend, Terminal};

    fn render(state: &AppState, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, state)).unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn brand_mark_appears_at_origin() {
        let state = AppState::new();
        let buf = render(&state, 80, 5);
        // Cell 0,0 is "▌"; cell 1,0 is "Z" with orange bg + black fg.
        let z = buf.cell((1, 0)).unwrap();
        assert_eq!(z.symbol(), "Z");
        assert_eq!(z.bg, BRAND_ORANGE);
        assert_eq!(z.fg, Color::Black);
        assert!(z.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn status_bar_shows_offline_dot_when_offline() {
        let mut state = AppState::new();
        state.connection = Connection::Offline("daemon down".to_string());
        let buf = render(&state, 80, 5);
        // Last row contains the status bar. Look for `○` somewhere on it.
        let last = 4u16; // h-1
        let row: String = (0..80).map(|x| buf.cell((x, last)).unwrap().symbol().to_string()).collect::<Vec<_>>().join("");
        assert!(row.contains("○"), "expected ○ in status row, got: {}", row);
    }

    #[test]
    fn filter_mode_shows_query_in_bottom_right() {
        let mut state = AppState::new();
        state.input_mode = InputMode::Filter;
        state.filter = "cla".to_string();
        let buf = render(&state, 80, 5);
        let last = 4u16;
        let row: String = (0..80).map(|x| buf.cell((x, last)).unwrap().symbol().to_string()).collect::<Vec<_>>().join("");
        assert!(row.contains("/cla"), "expected '/cla' in status row, got: {}", row);
    }

    #[test]
    fn live_state_shows_green_dot_and_counts() {
        let mut state = AppState::new();
        state.connection = Connection::Live;
        let buf = render(&state, 80, 5);
        let last = 4u16;
        let row: String = (0..80).map(|x| buf.cell((x, last)).unwrap().symbol().to_string()).collect::<Vec<_>>().join("");
        assert!(row.contains("●"));
        assert!(row.contains("0 tiles"));
        assert!(row.contains("0 notifs"));
    }

    use crate::events::tiles::tile::Tile;

    fn fake_tile(agent: &str, project: &str, surface: &str) -> Tile {
        Tile {
            id: format!("tile_{}", agent),
            agent: agent.to_string(),
            project_anchor: Some(project.to_string()),
            project_label: Some(project.to_string()),
            surface_kind: "cli".to_string(),
            surface_token: surface.to_string(),
            surface_label: surface.to_string(),
            first_seen_at: 0,
            last_seen_at: 1_000,
            event_count: 5,
            latest_event_type: "turn.completed".to_string(),
            focus_uri: Some("workspace://x".to_string()),
        }
    }

    #[test]
    fn tiles_list_shows_cursor_on_selected_row() {
        let mut state = AppState::new();
        state.tiles = vec![fake_tile("claude-code", "zestful", "tmux:z/pane:%0")];
        state.connection = Connection::Live;
        let buf = render(&state, 80, 10);
        // Body starts at row 1. The first tile row should have ▶ near the start.
        let row1: String = (0..40).map(|x| buf.cell((x, 2)).unwrap().symbol().to_string()).collect::<Vec<_>>().join("");
        assert!(row1.contains("▶"), "expected ▶ cursor, got: {}", row1);
        assert!(row1.contains("claude-code"));
    }

    #[test]
    fn detail_pane_shows_no_tile_selected_when_empty() {
        let state = AppState::new();
        let buf = render(&state, 80, 10);
        // Right pane spans cols ~28..80 (35% / 65% split of 80 cols).
        // Body occupies rows 1..9; top border at row 1, inner content starts at row 2.
        let mut found = false;
        for y in 1..9u16 {
            let row: String = (0..80).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect::<Vec<_>>().join("");
            if row.contains("No tile selected") { found = true; break; }
        }
        assert!(found, "expected 'No tile selected' somewhere in detail pane body rows");
    }

    #[test]
    fn detail_pane_shows_metadata_for_selected() {
        let mut state = AppState::new();
        state.tiles = vec![fake_tile("claude-code", "zestful", "tmux:z/pane:%0")];
        state.connection = Connection::Live;
        let buf = render(&state, 100, 12);
        // Look for surface_label somewhere in the right pane.
        let mut found = false;
        for y in 1..11 {
            let row: String = (0..100).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect::<Vec<_>>().join("");
            if row.contains("tmux:z/pane:%0") { found = true; break; }
        }
        assert!(found, "expected surface_label in detail pane");
    }
}
