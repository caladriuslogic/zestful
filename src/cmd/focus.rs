//! `zestful focus` — focus a terminal tab from the command line.
//!
//! Parses a `workspace://` URI (or explicit app/window/tab args) and activates
//! the target terminal tab. Runs the same focus logic as the daemon, but
//! directly from the CLI without an HTTP round-trip.

use anyhow::{bail, Result};

use crate::workspace::{browsers, ides, multiplexers, terminals, uri};

/// Execute the `focus` command.
pub fn run(
    terminal_uri: Option<String>,
    app: Option<String>,
    window_id: Option<String>,
    tab_id: Option<String>,
) -> Result<()> {
    let parsed = if let Some(ref uri_str) = terminal_uri {
        uri::parse_terminal_uri(uri_str)
            .ok_or_else(|| anyhow::anyhow!("invalid terminal URI: {}", uri_str))?
    } else if let Some(app) = app {
        if app.is_empty() {
            bail!("--app must not be empty");
        }
        uri::ParsedTerminalUri {
            app,
            window_id,
            tab_id,
            project_id: None,
            terminal_id: None,
            shelldon: None,
            tmux: None,
        }
    } else {
        bail!("provide a URI positional arg or --app\n\nUsage:\n  zestful focus workspace://iterm2/window:1/tab:2\n  zestful focus --app iTerm2 --tab-id 3");
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_lower = parsed.app.to_lowercase();
        let is_browser = app_lower.contains("chrome")
            || app_lower.contains("safari")
            || app_lower.contains("firefox");
        let is_ide = parsed.project_id.is_some()
            || parsed.terminal_id.is_some()
            || app_lower == "xcode"
            || app_lower == "vscode"
            || app_lower.contains("visual studio code")
            || app_lower == "cursor"
            || app_lower == "windsurf"
            || app_lower == "zed";
        let focus_result = if is_ide {
            ides::handle_focus(
                &parsed.app,
                parsed.project_id.as_deref(),
                parsed.terminal_id.as_deref(),
            ).await
        } else if is_browser {
            browsers::handle_focus(
                &parsed.app,
                parsed.window_id.as_deref(),
                parsed.tab_id.as_deref(),
            )
            .await
        } else {
            terminals::handle_focus(
                &parsed.app,
                parsed.window_id.as_deref(),
                parsed.tab_id.as_deref(),
            )
            .await
        };
        if let Err(e) = focus_result {
            crate::log::log("focus", &format!("focus error: {}", e));
            eprintln!("zestful: focus error: {}", e);
        }

        if let Some(ref shelldon) = parsed.shelldon {
            if let Err(e) = multiplexers::shelldon::focus(shelldon).await {
                crate::log::log("focus", &format!("shelldon focus error: {}", e));
                eprintln!("zestful: shelldon focus error: {}", e);
            }
        }

        if let Some(ref tmux) = parsed.tmux {
            if let Err(e) = multiplexers::tmux::focus(tmux).await {
                crate::log::log("focus", &format!("tmux focus error: {}", e));
                eprintln!("zestful: tmux focus error: {}", e);
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_no_args_errors() {
        let result = run(None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("provide a URI"));
    }

    #[test]
    fn test_run_empty_app_errors() {
        let result = run(None, Some(String::new()), None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_run_invalid_uri_errors() {
        let result = run(Some("not-a-uri".into()), None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid terminal URI"));
    }

    #[test]
    fn test_run_with_app() {
        // Should succeed (focus will be a no-op for a non-running app)
        let result = run(None, Some("SomeRandomApp".into()), None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_uri() {
        let result = run(Some("workspace://kitty/window:1/tab:2".into()), None, None, None);
        assert!(result.is_ok());
    }
}
