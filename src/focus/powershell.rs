//! PowerShell focus handler (Windows only).
//!
//! Uses `Microsoft.VisualBasic.Interaction::AppActivate` via `powershell.exe`
//! to bring a PowerShell window to the foreground. Targets by process ID
//! (window_id from the URI) when available, otherwise activates by name.

use anyhow::Result;

/// Focus a PowerShell window.
///
/// `window_id` is the process ID of the target PowerShell process, as
/// reported by workspace-inspector in the `workspace://` URI.
pub async fn focus(window_id: Option<&str>) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        tokio::task::spawn_blocking({
            let window_id = window_id.map(String::from);
            move || focus_sync(window_id.as_deref())
        })
        .await??;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window_id;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn focus_sync(window_id: Option<&str>) -> Result<()> {
    // AppActivate accepts either a process ID (integer) or a window title
    // string. Use the process ID when we have it for precise targeting.
    let target = match window_id {
        Some(pid) if pid.chars().all(|c| c.is_ascii_digit()) => pid.to_string(),
        _ => "\"PowerShell\"".to_string(),
    };

    let script = format!(
        "Add-Type -AssemblyName Microsoft.VisualBasic; \
         [Microsoft.VisualBasic.Interaction]::AppActivate({})",
        target
    );

    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_focus_no_panic() {
        let result = focus(None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_pid() {
        let result = focus(Some("1234")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_non_numeric_id() {
        // Non-numeric window_id falls back to name-based activation
        let result = focus(Some("some-id")).await;
        assert!(result.is_ok());
    }
}
