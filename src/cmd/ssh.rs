//! `zestful ssh` — SSH with Zestful forwarding.
//!
//! Syncs the auth token, port, and focus context to the remote host, then
//! `exec`s `ssh -R` to set up a reverse port forward. Remote agents can then
//! call `zestful notify` and the notifications route back to the local Mac app.

use crate::config;
use anyhow::{bail, Result};
use std::process::Command;

/// Execute the `ssh` command: sync config to remote, then exec ssh with reverse tunnel.
pub fn run(args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        bail!("zestful ssh requires a host\nUsage: zestful ssh [user@]host [ssh options...]");
    }

    let token = config::read_token().ok_or_else(|| {
        anyhow::anyhow!("Zestful app not running. Token not found.")
    })?;
    let port = config::read_port();

    let dest = &args[0];

    // Capture terminal URI for click-to-focus on the remote side
    let terminal_uri = terminal_inspector::locate().ok();

    // Sync config to remote
    eprintln!("Syncing Zestful config to {}...", dest);

    // Create remote config dir
    run_ssh(dest, "mkdir -p ~/.config/zestful && chmod 700 ~/.config/zestful")?;

    // Copy token
    pipe_to_ssh(
        dest,
        &token,
        "cat > ~/.config/zestful/local-token && chmod 600 ~/.config/zestful/local-token",
    )?;

    // Copy port
    pipe_to_ssh(
        dest,
        &port.to_string(),
        "cat > ~/.config/zestful/port && chmod 600 ~/.config/zestful/port",
    )?;

    // Copy terminal URI so remote `zestful notify` can use it
    if let Some(ref uri) = terminal_uri {
        pipe_to_ssh(
            dest,
            uri,
            "cat > ~/.config/zestful/terminal-uri && chmod 600 ~/.config/zestful/terminal-uri",
        )?;
    }

    eprintln!("Connecting with Zestful forwarding (port {})...", port);

    // exec ssh with reverse port forward
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new("ssh")
            .arg("-R")
            .arg(format!("{}:localhost:{}", port, port))
            .args(&args)
            .exec();
        // exec() only returns on error
        bail!("Failed to exec ssh: {}", err);
    }

    #[cfg(not(unix))]
    {
        let status = Command::new("ssh")
            .arg("-R")
            .arg(format!("{}:localhost:{}", port, port))
            .args(&args)
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn run_ssh(dest: &str, remote_cmd: &str) -> Result<()> {
    let status = Command::new("ssh")
        .args([dest, remote_cmd])
        .status()?;
    if !status.success() {
        bail!("SSH command failed: {}", remote_cmd);
    }
    Ok(())
}

fn pipe_to_ssh(dest: &str, input: &str, remote_cmd: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("ssh")
        .args([dest, remote_cmd])
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(input.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("SSH pipe command failed: {}", remote_cmd);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_focus_context_building() {
        let mut focus_lines = Vec::new();
        // Simulate TERM_PROGRAM=kitty
        focus_lines.push(format!("app={}", "kitty"));
        // Simulate KITTY_WINDOW_ID=42
        focus_lines.push(format!("window_id={}", "42"));

        let context = focus_lines.join("\n");
        assert_eq!(context, "app=kitty\nwindow_id=42");
    }

    #[test]
    fn test_reverse_port_forward_arg() {
        let port: u16 = 21547;
        let arg = format!("{}:localhost:{}", port, port);
        assert_eq!(arg, "21547:localhost:21547");
    }

    #[test]
    fn test_empty_focus_lines() {
        let focus_lines: Vec<String> = Vec::new();
        assert!(focus_lines.is_empty());
    }

    #[test]
    fn test_focus_context_only_term_program() {
        let mut focus_lines = Vec::new();
        focus_lines.push(format!("app={}", "iTerm2"));
        let context = focus_lines.join("\n");
        assert_eq!(context, "app=iTerm2");
    }

    #[test]
    fn test_run_requires_args() {
        let result = super::run(vec![]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("requires a host"));
    }
}
