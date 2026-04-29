//! IDE detection (Xcode, VS Code family, etc.)

#[cfg(target_os = "macos")]
pub mod vscode_family;
#[cfg(target_os = "macos")]
mod xcode;

#[cfg(target_os = "windows")]
pub mod vscode_family_windows;

use crate::workspace::types::IdeInstance;
use anyhow::Result;

pub fn detect_all() -> Result<Vec<IdeInstance>> {
    #[allow(unused_mut)]
    let mut ides = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(instance)) = xcode::detect() {
            ides.push(instance);
        }
        if let Ok(more) = vscode_family::detect_all() {
            ides.extend(more.into_iter().filter(|i| !i.projects.is_empty()));
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(more) = vscode_family_windows::detect_all() {
            ides.extend(more.into_iter().filter(|i| !i.projects.is_empty()));
        }
    }

    Ok(ides)
}

/// Dispatch focus to the right IDE handler. Called from the daemon when a
/// `workspace://<ide>/...` URI arrives. The URI may carry a `project:<name>`
/// (workspace-level focus) or a `terminal:<id>` (integrated terminal focus
/// via the Zestful VS Code extension).
pub async fn handle_focus(
    app: &str,
    window_id: Option<&str>,
    project_id: Option<&str>,
    terminal_id: Option<&str>,
) -> Result<()> {
    let lower = app.to_lowercase();

    #[cfg(target_os = "macos")]
    {
        if lower == "vscode" || lower.contains("visual studio code") {
            if let Some(tid) = terminal_id {
                return vscode_family::focus_terminal(Family::VSCode, tid).await;
            }
            return vscode_family::focus(Family::VSCode, project_id).await;
        }
        if lower == "cursor" {
            if let Some(tid) = terminal_id {
                return vscode_family::focus_terminal(Family::Cursor, tid).await;
            }
            return vscode_family::focus(Family::Cursor, project_id).await;
        }
        if lower == "windsurf" {
            if let Some(tid) = terminal_id {
                return vscode_family::focus_terminal(Family::Windsurf, tid).await;
            }
            return vscode_family::focus(Family::Windsurf, project_id).await;
        }
        if lower == "xcode" {
            return xcode_focus(project_id).await;
        }
    }
    #[cfg(target_os = "windows")]
    {
        use vscode_family_windows::Family;
        let family = if lower == "vscode" || lower == "code" || lower.contains("visual studio code") {
            Some(Family::VSCode)
        } else if lower == "cursor" {
            Some(Family::Cursor)
        } else if lower == "windsurf" {
            Some(Family::Windsurf)
        } else {
            None
        };
        if let Some(fam) = family {
            if let Some(wid) = window_id {
                return vscode_family_windows::focus_by_pid(fam, wid, project_id).await;
            }
            if let Some(tid) = terminal_id {
                return vscode_family_windows::focus_terminal(fam, tid).await;
            }
            return vscode_family_windows::focus(fam, project_id).await;
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (lower, window_id, project_id, terminal_id);

    // Generic fallback: just activate the app by name.
    crate::workspace::uri::activate_generic(app).await
}

#[cfg(target_os = "macos")]
pub use vscode_family::Family;

#[cfg(target_os = "windows")]
pub use vscode_family_windows::Family;

#[cfg(target_os = "macos")]
async fn xcode_focus(_project_id: Option<&str>) -> Result<()> {
    // No per-project Xcode focus yet — just activate the app.
    crate::workspace::uri::activate_generic("Xcode").await
}
