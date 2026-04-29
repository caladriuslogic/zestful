//! ratatui rendering. Pure functions over `&AppState`.

use crate::cmd::top::app::{AppState, Connection, Pane};
use crate::cmd::top::colors::{BRAND_ORANGE, BRAND_ORANGE_LIGHT};
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
    // body — filled in by Tasks 9–10
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
    // Left side: connection state + counts.
    let (dot, dot_color, label) = match &state.connection {
        Connection::Live           => ("●", Color::Rgb(0x10, 0xB9, 0x81), "live"),
        Connection::Reconnecting   => ("◐", Color::Rgb(0xEA, 0xB3, 0x08), "reconnecting…"),
        Connection::Offline(_)     => ("○", Color::Rgb(0xEF, 0x44, 0x44), "offline"),
    };
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
}
