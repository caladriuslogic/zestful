//! `zestful notify` — emit a structured `agent.notified` event to the daemon.
//!
//! Auto-captures the terminal URI via the built-in workspace inspector for
//! click-to-focus. Best-effort: event-emission failures are logged and
//! swallowed — failure to reach the daemon must never break the CLI.

use crate::config;
use crate::events::severity::Severity;
use anyhow::Result;

/// Execute the `notify` command: locate terminal, emit `agent.notified` event.
pub fn run(
    agent: String,
    message: String,
    severity: String,
    terminal_uri: Option<String>,
    no_push: bool,
    debug: bool,
) -> Result<()> {
    // Use explicit URI if provided, otherwise auto-detect via workspace inspector,
    // falling back to saved URI file (written by `zestful ssh` for remote sessions).
    let terminal_uri = terminal_uri
        .or_else(|| crate::workspace::locate().ok())
        .or_else(|| config::read_terminal_uri());

    crate::log::log(
        "notify",
        &format!(
            "agent={} severity={} message=\"{}\" uri={} push={}",
            agent,
            severity,
            message,
            terminal_uri.as_deref().unwrap_or("none"),
            !no_push
        ),
    );

    if debug {
        eprintln!("zestful: uri={}", terminal_uri.as_deref().unwrap_or("none"));
    }

    // Map --severity (info|warning|urgent) → Severity enum, passing as a hint.
    // The clap default is "warning" so we always send Some.
    let severity_hint = match severity.as_str() {
        "info" => Some(Severity::Info),
        "warning" => Some(Severity::Warn),
        "urgent" => Some(Severity::Urgent),
        _ => None,
    };
    let push_hint = if no_push { Some(false) } else { None };

    let envelopes = crate::events::map_cli_notify(
        &agent,
        &message,
        terminal_uri,
        severity_hint,
        push_hint,
    );
    if let Err(e) = crate::events::send_to_daemon(&envelopes) {
        crate::log::log("notify", &format!("event emission failed: {}", e));
    }

    Ok(())
}
