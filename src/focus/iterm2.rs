//! iTerm2 focus handler (macOS only).
//!
//! Uses the [`iterm2-client`](https://crates.io/crates/iterm2-client) crate for
//! native WebSocket + Protobuf communication with iTerm2 — no Python dependency.
//! Connects via Unix socket. Falls back to AppleScript activation if the API is
//! unavailable.

use anyhow::Result;

/// Focus an iTerm2 window/tab using the native iterm2-client crate.
///
/// `tab_id` is a 1-based tab index from the terminal:// URI.
pub async fn focus(window_id: Option<&str>, tab_id: Option<&str>) -> Result<()> {
    if tab_id.is_some() {
        if let Err(e) = focus_via_api(tab_id).await {
            eprintln!("[zestfuld] iTerm2 API error (falling back to AppleScript): {}", e);
        }
    }

    // Always bring iTerm2 to front
    super::activate_app_sync("iTerm2");

    // Suppress unused warning — window_id from the URI doesn't map to iTerm2's
    // internal UUID-based window IDs, so we focus by tab index instead.
    let _ = window_id;

    Ok(())
}

async fn focus_via_api(tab_id: Option<&str>) -> Result<()> {
    use iterm2_client::{App, Connection};

    let conn = Connection::connect("zestful-daemon").await?;
    let app = App::new(conn);
    let sessions = app.list_sessions().await?;

    let tab_id = match tab_id {
        Some(id) => id,
        None => return Ok(()),
    };

    // tab_id from the URI is a 1-based index (e.g. "1" = first tab).
    // iTerm2 tab IDs are sequential integers (e.g. 2, 3, 4) that don't
    // correspond to position. Use the index to select by position.
    if let Ok(tab_idx) = tab_id.parse::<usize>() {
        let zero_idx = tab_idx.saturating_sub(1);
        for window in &sessions.windows {
            if let Some(tab_info) = window.tabs.get(zero_idx) {
                tab_info.tab.activate().await?;
                return Ok(());
            }
        }
    }

    // Fall back: try matching tab_id against iTerm2's internal tab ID string
    for window in &sessions.windows {
        for tab_info in &window.tabs {
            if tab_info.tab.id == tab_id {
                tab_info.tab.activate().await?;
                return Ok(());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_focus_no_ids() {
        let result = focus(None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_ids_falls_back() {
        let result = focus(Some("99999"), Some("1")).await;
        assert!(result.is_ok());
    }
}
