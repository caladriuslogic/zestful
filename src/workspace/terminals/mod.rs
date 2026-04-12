mod alacritty;
mod ghostty;
pub mod kitty;
pub mod wezterm;

#[cfg(target_os = "macos")]
mod apple_terminal;
#[cfg(target_os = "macos")]
pub mod iterm2;

#[cfg(target_os = "linux")]
mod gnome_terminal;

#[cfg(target_os = "windows")]
pub mod cmd;
#[cfg(target_os = "windows")]
pub mod powershell;
#[cfg(target_os = "windows")]
pub mod windows_terminal;

#[cfg(all(test, target_os = "windows"))]
mod windows_tests;

use anyhow::Result;

use crate::workspace::types::TerminalEmulator;
use crate::workspace::uri;

pub fn detect_all() -> Result<Vec<TerminalEmulator>> {
    let mut terminals = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(t)) = iterm2::detect() {
            terminals.push(t);
        }

        if let Ok(Some(t)) = apple_terminal::detect() {
            terminals.push(t);
        }
    }

    if let Ok(Some(t)) = kitty::detect() {
        terminals.push(t);
    }

    if let Ok(Some(t)) = ghostty::detect() {
        terminals.push(t);
    }

    if let Ok(Some(t)) = wezterm::detect() {
        terminals.push(t);
    }

    if let Ok(Some(t)) = alacritty::detect() {
        terminals.push(t);
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(Some(t)) = gnome_terminal::detect() {
            terminals.push(t);
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Collect shell PIDs already captured by Windows Terminal so that
        // classic detectors can skip them and avoid duplicate entries.
        // On Windows 10 a user can have both Windows Terminal tabs and
        // standalone classic console windows open at the same time, so we
        // always run all three detectors rather than using an if/else.
        let wt_pids: std::collections::HashSet<u32> =
            match windows_terminal::detect() {
                Ok(Some(t)) => {
                    let pids = t
                        .windows
                        .iter()
                        .flat_map(|w| w.tabs.iter())
                        .filter_map(|tab| tab.shell_pid)
                        .collect();
                    terminals.push(t);
                    pids
                }
                _ => std::collections::HashSet::new(),
            };

        if let Ok(Some(mut t)) = cmd::detect() {
            t.windows.retain(|w| {
                !w.tabs
                    .iter()
                    .any(|tab| tab.shell_pid.map_or(false, |p| wt_pids.contains(&p)))
            });
            if !t.windows.is_empty() {
                terminals.push(t);
            }
        }

        if let Ok(Some(mut t)) = powershell::detect() {
            t.windows.retain(|w| {
                !w.tabs
                    .iter()
                    .any(|tab| tab.shell_pid.map_or(false, |p| wt_pids.contains(&p)))
            });
            if !t.windows.is_empty() {
                terminals.push(t);
            }
        }
    }

    Ok(terminals)
}

/// Dispatch focus to the appropriate terminal handler.
pub async fn handle_focus(app: &str, window_id: Option<&str>, tab_id: Option<&str>) -> Result<()> {
    uri::validate_focus_id(app, "app")?;
    if let Some(wid) = window_id {
        uri::validate_focus_id(wid, "window_id")?;
    }
    if let Some(tid) = tab_id {
        uri::validate_focus_id(tid, "tab_id")?;
    }

    let lower = app.to_lowercase();

    if lower == "windows terminal" {
        #[cfg(target_os = "windows")]
        {
            match window_id {
                Some(wid) => windows_terminal::focus(wid, tab_id).await,
                None => Ok(()),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    } else if lower == "cmd" {
        #[cfg(target_os = "windows")]
        {
            cmd::focus(window_id).await
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    } else if lower.contains("kitty") {
        kitty::focus(window_id, tab_id).await
    } else if lower.contains("iterm") {
        #[cfg(target_os = "macos")]
        {
            iterm2::focus(window_id, tab_id).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            uri::activate_generic(app).await
        }
    } else if lower.contains("powershell") {
        #[cfg(target_os = "windows")]
        {
            powershell::focus(window_id).await
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    } else if lower.contains("wezterm") {
        wezterm::focus(tab_id).await
    } else if lower.contains("terminal") {
        #[cfg(target_os = "macos")]
        {
            apple_terminal::focus(window_id, tab_id).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            uri::activate_generic(app).await
        }
    } else {
        uri::activate_generic(app).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_focus_dispatches_kitty() {
        let result = handle_focus("kitty", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_windows_terminal() {
        let result = handle_focus("Windows Terminal", Some("99999"), Some("1")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_cmd() {
        let result = handle_focus("Cmd", Some("1234"), None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_powershell() {
        let result = handle_focus("PowerShell", Some("1234"), None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_wezterm() {
        let result = handle_focus("WezTerm", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_terminal() {
        let result = handle_focus("Terminal", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_dispatches_generic() {
        let result = handle_focus("SomeRandomApp", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_case_insensitive() {
        let result = handle_focus("KITTY", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_focus_rejects_invalid_app() {
        let result = handle_focus("bad\"app", None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_focus_rejects_invalid_tab_id() {
        let result = handle_focus("kitty", None, Some("tab$(whoami)")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_focus_rejects_invalid_window_id() {
        let result = handle_focus("kitty", Some("win`id`"), None).await;
        assert!(result.is_err());
    }
}
