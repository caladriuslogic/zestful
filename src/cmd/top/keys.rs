//! Key bindings and the `Action` enum. Single source of truth for both
//! the `KeyEvent → Option<Action>` mapping AND the `?` help overlay
//! contents — they cannot drift.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,

    // Navigation
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    PageDown,
    PageUp,
    FocusNextPane,
    FocusPrevPane,

    // Filter mode (text entry)
    EnterFilterMode,
    FilterChar(char),
    FilterBackspace,
    CommitFilter,
    ExitFilterMode,

    // Actions
    Refresh,
    Focus,

    // Display toggles
    CycleSort,
    ToggleNotifOnly,
    ToggleHelp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Filter,
}

/// Rows for the `?` help overlay. Order is the order shown.
#[derive(Debug, Clone, Copy)]
pub struct HelpRow {
    pub section: &'static str,
    pub keys: &'static str,
    #[allow(dead_code)] // consumed by ui.rs help overlay in Task 10
    pub description: &'static str,
}

pub const HELP: &[HelpRow] = &[
    HelpRow { section: "Navigation", keys: "↑/↓ or k/j",   description: "navigate tiles list" },
    HelpRow { section: "Navigation", keys: "Tab/Shift-Tab", description: "switch focus between tiles list and detail pane" },
    HelpRow { section: "Navigation", keys: "PgUp/PgDn",     description: "page through long lists" },
    HelpRow { section: "Navigation", keys: "g / G",         description: "first / last tile" },

    HelpRow { section: "Actions",    keys: "Enter",         description: "focus the selected tile (POST /focus)" },
    HelpRow { section: "Actions",    keys: "/",             description: "start filtering (fuzzy substring)" },
    HelpRow { section: "Actions",    keys: "Esc",           description: "close help · clear filter · cancel" },
    HelpRow { section: "Actions",    keys: "r",             description: "force-refresh now (in addition to live SSE)" },
    HelpRow { section: "Actions",    keys: "?",             description: "toggle this help overlay" },
    HelpRow { section: "Actions",    keys: "q / Ctrl-C",    description: "quit" },

    HelpRow { section: "Display",    keys: "s",             description: "cycle sort: last_seen↓, event_count↓, agent↑, ctx%↓, $↓" },
    HelpRow { section: "Display",    keys: "N",             description: "toggle 'only tiles with notifications'" },
];

/// Map a key event to an `Action`. Returns `None` for keys we don't bind.
/// `mode` selects between Normal and Filter input modes — most bindings
/// are inactive while typing into the filter.
pub fn key_to_action(ev: KeyEvent, mode: InputMode) -> Option<Action> {
    if ev.modifiers.contains(KeyModifiers::CONTROL) && ev.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }

    match mode {
        InputMode::Filter => match ev.code {
            KeyCode::Esc        => Some(Action::ExitFilterMode),
            KeyCode::Enter      => Some(Action::CommitFilter),
            KeyCode::Backspace  => Some(Action::FilterBackspace),
            KeyCode::Char(c)    => Some(Action::FilterChar(c)),
            _                   => None,
        },
        InputMode::Normal => match ev.code {
            KeyCode::Char('q')  => Some(Action::Quit),
            KeyCode::Up         => Some(Action::SelectPrev),
            KeyCode::Down       => Some(Action::SelectNext),
            KeyCode::Char('k')  => Some(Action::SelectPrev),
            KeyCode::Char('j')  => Some(Action::SelectNext),
            KeyCode::Char('g')  => Some(Action::SelectFirst),
            KeyCode::Char('G')  => Some(Action::SelectLast),
            KeyCode::PageUp     => Some(Action::PageUp),
            KeyCode::PageDown   => Some(Action::PageDown),
            KeyCode::Tab        => Some(Action::FocusNextPane),
            KeyCode::BackTab    => Some(Action::FocusPrevPane),
            KeyCode::Enter      => Some(Action::Focus),
            KeyCode::Char('/')  => Some(Action::EnterFilterMode),
            KeyCode::Esc        => Some(Action::ExitFilterMode),
            KeyCode::Char('r')  => Some(Action::Refresh),
            KeyCode::Char('?')  => Some(Action::ToggleHelp),
            KeyCode::Char('s')  => Some(Action::CycleSort),
            KeyCode::Char('N')  => Some(Action::ToggleNotifOnly),
            _                   => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn k(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }

    #[test]
    fn quit_on_q_or_ctrl_c() {
        assert_eq!(key_to_action(k(KeyCode::Char('q')), InputMode::Normal), Some(Action::Quit));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(key_to_action(ctrl_c, InputMode::Normal), Some(Action::Quit));
        assert_eq!(key_to_action(ctrl_c, InputMode::Filter), Some(Action::Quit));
    }

    #[test]
    fn navigation_keys_in_normal_mode() {
        assert_eq!(key_to_action(k(KeyCode::Up), InputMode::Normal), Some(Action::SelectPrev));
        assert_eq!(key_to_action(k(KeyCode::Down), InputMode::Normal), Some(Action::SelectNext));
        assert_eq!(key_to_action(k(KeyCode::Char('j')), InputMode::Normal), Some(Action::SelectNext));
        assert_eq!(key_to_action(k(KeyCode::Char('k')), InputMode::Normal), Some(Action::SelectPrev));
        assert_eq!(key_to_action(k(KeyCode::Char('g')), InputMode::Normal), Some(Action::SelectFirst));
        assert_eq!(key_to_action(k(KeyCode::Char('G')), InputMode::Normal), Some(Action::SelectLast));
    }

    #[test]
    fn filter_mode_consumes_chars_and_handles_backspace() {
        assert_eq!(key_to_action(k(KeyCode::Char('a')), InputMode::Filter), Some(Action::FilterChar('a')));
        assert_eq!(key_to_action(k(KeyCode::Backspace), InputMode::Filter), Some(Action::FilterBackspace));
        assert_eq!(key_to_action(k(KeyCode::Esc), InputMode::Filter), Some(Action::ExitFilterMode));
        // Bare 'c' in filter mode is just text, not Ctrl-C (verifies the
        // early-return for Ctrl-C doesn't shadow normal char input).
        assert_eq!(key_to_action(k(KeyCode::Char('c')), InputMode::Filter), Some(Action::FilterChar('c')));
        // 'q' in filter mode is text, not Quit.
        assert_eq!(key_to_action(k(KeyCode::Char('q')), InputMode::Filter), Some(Action::FilterChar('q')));
    }

    #[test]
    fn slash_enters_filter_mode_in_normal() {
        assert_eq!(key_to_action(k(KeyCode::Char('/')), InputMode::Normal), Some(Action::EnterFilterMode));
        // In filter mode, '/' is just another char.
        assert_eq!(key_to_action(k(KeyCode::Char('/')), InputMode::Filter), Some(Action::FilterChar('/')));
    }

    #[test]
    fn enter_in_filter_mode_commits_not_exits() {
        assert_eq!(key_to_action(k(KeyCode::Enter), InputMode::Filter), Some(Action::CommitFilter));
    }

    #[test]
    fn unbound_keys_return_none() {
        assert_eq!(key_to_action(k(KeyCode::Char('z')), InputMode::Normal), None);
    }

    #[test]
    fn question_mark_toggles_help_in_normal() {
        assert_eq!(key_to_action(k(KeyCode::Char('?')), InputMode::Normal), Some(Action::ToggleHelp));
    }

    #[test]
    fn help_table_covers_all_normal_actions() {
        // Every key bound in Normal mode should appear in at least one HELP row.
        // Tests the invariant: keys.rs is the single source of truth — if you add a
        // binding to key_to_action, you must add a HELP row for it.
        let bound_keys = [
            "↑", "↓", "k", "j", "g", "G", "PgUp", "PgDn",
            "Tab", "Shift-Tab", "Enter", "/", "Esc",
            "r", "?", "q", "Ctrl-C", "s", "N",
        ];
        for k in bound_keys {
            assert!(
                HELP.iter().any(|r| r.keys.contains(k)),
                "no HELP row mentions key {:?}", k,
            );
        }
        // Sections still required.
        assert!(HELP.iter().any(|r| r.section == "Navigation"));
        assert!(HELP.iter().any(|r| r.section == "Actions"));
        assert!(HELP.iter().any(|r| r.section == "Display"));
        // No empty descriptions.
        assert!(HELP.iter().all(|r| !r.description.is_empty()),
                "HELP rows must all have non-empty descriptions");
    }
}
