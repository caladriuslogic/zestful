//! Browser detection (Chrome, Safari, etc.)

#[cfg(target_os = "macos")]
mod chrome;

use anyhow::Result;
use crate::workspace::types::BrowserInstance;

/// Focus a browser tab by app name, window ID, and tab index.
pub async fn handle_focus(app: &str, window_id: Option<&str>, tab_id: Option<&str>) -> Result<()> {
    let lower = app.to_lowercase();
    let win_id = window_id.unwrap_or("");
    let tab_index: u32 = tab_id.and_then(|t| t.parse().ok()).unwrap_or(1);

    if lower.contains("chrome") {
        #[cfg(target_os = "macos")]
        {
            chrome::focus(win_id, tab_index).await?;
        }
    } else {
        // Generic: just activate the app
        crate::workspace::uri::activate_generic(app).await?;
    }

    Ok(())
}

pub fn detect_all() -> Result<Vec<BrowserInstance>> {
    let mut browsers = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(instance)) = chrome::detect() {
            browsers.push(instance);
        }
    }

    Ok(browsers)
}
