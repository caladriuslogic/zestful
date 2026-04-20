//! IDE detection (Xcode, VS Code family, etc.)

#[cfg(target_os = "macos")]
mod xcode;
#[cfg(target_os = "macos")]
pub mod vscode_family;

use anyhow::Result;
use crate::workspace::types::IdeInstance;

pub fn detect_all() -> Result<Vec<IdeInstance>> {
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

    Ok(ides)
}

/// Dispatch focus to the right IDE handler. Called from the daemon when a
/// `workspace://<ide>/...` URI arrives. The URI may carry a `project:<name>`
/// (workspace-level focus) or a `terminal:<id>` (integrated terminal focus
/// via the Zestful VS Code extension).
pub async fn handle_focus(app: &str, project_id: Option<&str>, terminal_id: Option<&str>) -> Result<()> {
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
    #[cfg(not(target_os = "macos"))]
    let _ = (lower, project_id, terminal_id);

    // Generic fallback: just activate the app by name.
    crate::workspace::uri::activate_generic(app).await
}

#[cfg(target_os = "macos")]
pub use vscode_family::Family;

#[cfg(target_os = "macos")]
async fn xcode_focus(_project_id: Option<&str>) -> Result<()> {
    // No per-project Xcode focus yet — just activate the app.
    crate::workspace::uri::activate_generic("Xcode").await
}
