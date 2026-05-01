//! `zestful watch` — run a command and notify when it finishes.
//!
//! Spawns the command as a child process, captures the exit code, and sends a
//! notification with severity based on success/failure. Auto-captures terminal
//! URI via the built-in workspace inspector for click-to-focus.

use anyhow::{bail, Result};
use std::process::Command;

/// Execute the `watch` command: run child process, then send notification.
pub fn run(agent: String, command: Vec<String>) -> Result<()> {
    if command.is_empty() {
        bail!("zestful watch requires a command");
    }

    // Capture terminal URI before running the command (environment is stable now)
    let terminal_uri = crate::workspace::locate().ok();

    crate::log::log("watch", &format!("running: {}", command.join(" ")));

    // Run the command
    let status = Command::new(&command[0]).args(&command[1..]).status();

    let exit_code = match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            crate::log::log("watch", &format!("failed to run '{}': {}", command[0], e));
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
        (
            "urgent",
            format!("{} failed (exit {})", cmd_name, exit_code),
        )
    };

    crate::log::log(
        "watch",
        &format!(
            "exit={} severity={} agent={}",
            exit_code, severity, agent_name
        ),
    );
    // Emit a structured watch.completed event to the daemon. Best-effort;
    // event-emission failures are logged and swallowed.
    let envelopes = crate::events::map_watch_completed(
        &agent_name,
        &cmd_name,
        exit_code,
        None,  // duration_ms — not tracked today; future addition
        terminal_uri,
    );
    if let Err(e) = crate::events::send_to_daemon(&envelopes) {
        crate::log::log("watch", &format!("event emission failed: {}", e));
    }

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
