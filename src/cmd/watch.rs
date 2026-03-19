//! `zestful watch` — run a command and notify when it finishes.
//!
//! Spawns the command as a child process, captures the exit code, and sends a
//! notification with severity based on success/failure. Auto-detects the terminal
//! for click-to-focus, looking through multiplexers like tmux.

use crate::{cmd::notify, config};
use anyhow::{bail, Result};
use std::process::Command;

/// Execute the `watch` command: run child process, then send notification.
pub fn run(agent: String, command: Vec<String>) -> Result<()> {
    if command.is_empty() {
        bail!("zestful watch requires a command");
    }

    let token = config::read_token().ok_or_else(|| {
        anyhow::anyhow!("Zestful app not running or not configured. Token not found.")
    })?;
    let port = config::read_port();

    // Auto-detect terminal for click-to-focus (looks through tmux/screen)
    let app = config::detect_terminal();

    // Run the command
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status();

    let exit_code = match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("Failed to run '{}': {}", command[0], e);
            127
        }
    };

    let cmd_name = std::path::Path::new(&command[0])
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| command[0].clone());

    let agent_name = format!("{}:{}", agent, cmd_name);

    let (severity, message) = if exit_code == 0 {
        ("warning", format!("{} finished", cmd_name))
    } else {
        ("urgent", format!("{} failed (exit {})", cmd_name, exit_code))
    };

    // Send notification directly (not via HTTP to self)
    let _ = notify::send(&token, port, &agent_name, &message, severity, app, None, None, false);

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cmd_name_extraction() {
        let path = std::path::Path::new("/usr/bin/sleep");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, "sleep");
    }

    #[test]
    fn test_cmd_name_no_path() {
        let path = std::path::Path::new("npm");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, "npm");
    }

    #[test]
    fn test_agent_name_format() {
        let agent = "watch";
        let cmd_name = "npm";
        let agent_name = format!("{}:{}", agent, cmd_name);
        assert_eq!(agent_name, "watch:npm");
    }

    #[test]
    fn test_severity_on_success() {
        let exit_code = 0;
        let severity = if exit_code == 0 { "warning" } else { "urgent" };
        assert_eq!(severity, "warning");
    }

    #[test]
    fn test_severity_on_failure() {
        let exit_code = 1;
        let severity = if exit_code == 0 { "warning" } else { "urgent" };
        assert_eq!(severity, "urgent");
    }

    #[test]
    fn test_message_on_success() {
        let cmd_name = "build";
        let exit_code = 0;
        let message = if exit_code == 0 {
            format!("{} finished", cmd_name)
        } else {
            format!("{} failed (exit {})", cmd_name, exit_code)
        };
        assert_eq!(message, "build finished");
    }

    #[test]
    fn test_message_on_failure() {
        let cmd_name = "build";
        let exit_code = 42;
        let message = if exit_code == 0 {
            format!("{} finished", cmd_name)
        } else {
            format!("{} failed (exit {})", cmd_name, exit_code)
        };
        assert_eq!(message, "build failed (exit 42)");
    }
}
