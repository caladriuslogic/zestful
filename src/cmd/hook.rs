//! `zestful hook` — the universal agent hook entry point.
//!
//! Reads a JSON payload on stdin (in whatever schema the invoking agent
//! provides), detects which agent kind is calling us, maps the event to a
//! severity / message / push policy, and sends the notification via the
//! same path as `zestful notify`.

use anyhow::Result;
use std::io::Read;
use std::path::Path;

/// Execute the `hook` subcommand.
pub fn run(agent_override: Option<String>) -> Result<()> {
    // Read all of stdin so we can log and parse.
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;

    let preview: String = raw.chars().take(500).collect();
    crate::log::log("hook", &format!("stdin ({} bytes): {}", raw.len(), preview));

    if let Some(agent) = agent_override.as_deref() {
        crate::log::log("hook", &format!("--agent override: {}", agent));
    }

    let payload: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        crate::log::log("hook", &format!("JSON parse error: {}", e));
        serde_json::Value::Null
    });

    let agent_kind = crate::hooks::detect_agent(agent_override.as_deref(), &payload);
    let policy = crate::hooks::resolve_policy(agent_kind, &payload);
    crate::log::log(
        "hook",
        &format!(
            "resolved: agent={:?} event={} → severity={} msg={:?} push={} skip={}",
            agent_kind,
            payload
                .get("hook_event_name")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            policy.severity.as_str(),
            policy.message,
            policy.push,
            policy.skip,
        ),
    );

    if policy.skip {
        return Ok(());
    }

    // Compose the agent identifier: `<slug>:<project>` where project is the
    // basename of the payload's cwd. Cursor sends `workspace_roots[0]` instead
    // of `cwd`; fall back to that, then to our own PWD.
    let project_from_path = |p: &str| -> Option<String> {
        Path::new(p)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    };
    let project = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(project_from_path)
        .or_else(|| {
            payload
                .get("workspace_roots")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .and_then(project_from_path)
        })
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        })
        .unwrap_or_default();
    // Codex needs extra logic: the same hook config is shared by the Codex
    // desktop app and the `codex` CLI run from an editor terminal. The app
    // puts each task under `~/Documents/Codex/<task-folder>/`, while the CLI
    // inherits whatever workspace folder the terminal is in.
    let is_codex_desktop_app = agent_kind == crate::hooks::AgentKind::CodexCli
        && payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|c| c.contains("/Documents/Codex/"))
            .unwrap_or(false);

    // Codex.app fires the same hook regardless of whether the user is
    // driving it from the desktop app or the Codex VS Code extension (which
    // just proxies to the same daemon). If our VS Code extension reports an
    // active Codex conversation tab, we re-route this event to that editor
    // window. Otherwise we keep it on the Codex desktop app.
    let codex_editor = if is_codex_desktop_app {
        crate::workspace::find_active_codex_editor()
    } else {
        None
    };
    if let Some((slug, project, _)) = &codex_editor {
        crate::log::log(
            "hook",
            &format!(
                "codex correlation: routing to {}/project:{} (active tab)",
                slug, project
            ),
        );
    }

    // Codex desktop app has no per-window focus, so per-task tiles would
    // mislead — clicking one lands on whatever Codex window is frontmost.
    // Collapse to a single `codex` tile until per-window focus exists.
    let agent_name = if let Some((_, project, _)) = &codex_editor {
        format!("Codex CLI: {}", project)
    } else if is_codex_desktop_app {
        agent_kind.slug().to_string()
    } else if agent_kind == crate::hooks::AgentKind::CodexCli && !project.is_empty() {
        format!("Codex CLI: {}", project)
    } else if project.is_empty() {
        agent_kind.slug().to_string()
    } else {
        format!("{}:{}", agent_kind.slug(), project)
    };

    // Locate where we are (terminal/IDE/browser). If `locate()` can't match
    // (common when the agent's hook subprocess isn't a child of the editor
    // process we know about — e.g. Cursor's AI agent), fall back to a
    // project-level URI synthesized from the payload for IDE-family agents.
    let terminal_uri = crate::workspace::locate().ok().or_else(|| {
        // Codex.app fired, but a VS Code-family window has an active Codex
        // tab — route focus to that window instead of Codex.app.
        if let Some((slug, project, window_pid)) = &codex_editor {
            return Some(format!("workspace://{}/window:{}/project:{}", slug, window_pid, project));
        }
        // Cursor hook: synthesize a workspace-level URI when the hook's
        // parent chain doesn't reach the Cursor extension host.
        if agent_kind == crate::hooks::AgentKind::Cursor && !project.is_empty() {
            return Some(format!("workspace://cursor/project:{}", project));
        }
        // Codex desktop app with no editor correlation: app-level activation
        // only (no per-window focus).
        if is_codex_desktop_app {
            return Some("workspace://codex".to_string());
        }
        None
    });

    let token = crate::config::read_token().ok_or_else(|| {
        anyhow::anyhow!("Zestful app not running or not configured. Token not found.")
    })?;
    let port = crate::config::read_port();

    crate::log::log(
        "hook",
        &format!(
            "notify: agent={} message={:?} severity={} uri={} push={}",
            agent_name,
            policy.message,
            policy.severity.as_str(),
            terminal_uri.as_deref().unwrap_or("none"),
            policy.push,
        ),
    );

    crate::cmd::notify::send(
        &token,
        port,
        &agent_name,
        &policy.message,
        policy.severity.as_str(),
        terminal_uri.clone(),
        !policy.push,
    )?;

    // Also emit structured events to the daemon. Best-effort — errors never
    // propagate. This path runs independently of the legacy /notify path.
    let envelopes = crate::events::map_hook_payload(agent_kind, &payload, terminal_uri);
    if !envelopes.is_empty() {
        if let Err(e) = crate::events::send_to_daemon(&envelopes) {
            crate::log::log("hook", &format!("event emission failed: {}", e));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::events::map_hook_payload;
    use crate::hooks::AgentKind;
    use serde_json::json;

    #[test]
    fn hook_canned_claude_code_user_prompt_produces_event() {
        let payload = json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "write a test",
            "cwd": "/tmp/proj",
            "session_id": "sess_1",
        });
        let envs = map_hook_payload(AgentKind::ClaudeCode, &payload, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "turn.prompt_submitted");
        assert_eq!(envs[0].source, "claude-code");
        assert_eq!(
            envs[0].payload["prompt_preview"].as_str().unwrap(),
            "write a test"
        );
        // correlation.session_id flows through.
        let corr = envs[0].correlation.as_ref().unwrap();
        assert_eq!(corr.session_id.as_deref(), Some("sess_1"));
    }

    #[test]
    fn hook_canned_cursor_before_read_file_produces_no_events() {
        let payload = json!({
            "hook_event_name": "beforeReadFile",
            "path": "/etc/passwd",
        });
        let envs = map_hook_payload(AgentKind::Cursor, &payload, None);
        assert!(envs.is_empty());
    }
}
