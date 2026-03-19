//! iTerm2 focus handler (macOS only).
//!
//! Uses the [`iterm2-client`](https://crates.io/crates/iterm2-client) crate for
//! native WebSocket + Protobuf communication with iTerm2 — no Python dependency.
//! Connects via Unix socket. Falls back to AppleScript activation if the API is
//! unavailable.

use anyhow::Result;

/// Focus an iTerm2 tab using the native iterm2-client crate.
pub async fn focus(tab_id: Option<&str>) -> Result<()> {
    if let Some(tab_id) = tab_id {
        if let Err(e) = focus_via_api(tab_id).await {
            eprintln!("[zestfuld] iTerm2 API error (falling back to AppleScript): {}", e);
        }
    }

    // Always bring iTerm2 to front
    super::activate_app_sync("iTerm2");

    Ok(())
}

async fn focus_via_api(tab_id: &str) -> Result<()> {
    use iterm2_client::{App, Connection};

    let conn = Connection::connect_unix("zestful-daemon").await?;
    let app = App::new(conn);
    let sessions = app.list_sessions().await?;

    for window in &sessions.windows {
        for tab in &window.tabs {
            for session in &tab.sessions {
                // Query tab.title — the actual tab name visible in iTerm2.
                // The session.title is often "tmux" or "bash", not the tab name.
                // iTerm2 returns variable values as JSON-encoded strings,
                // so tab.title comes back as "\"Zestful\"" — strip the quotes.
                let tab_title_raw = session
                    .get_variable("tab.title")
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let tab_title = tab_title_raw.trim_matches('"');

                let matches = tab_title.eq_ignore_ascii_case(tab_id)
                    || session
                        .title
                        .as_deref()
                        .map(|t| t.eq_ignore_ascii_case(tab_id))
                        .unwrap_or(false);

                if matches {
                    session.activate().await?;
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_focus_no_tab_id() {
        let result = focus(None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_tab_id_falls_back() {
        let result = focus(Some("test-tab")).await;
        assert!(result.is_ok());
    }
}
