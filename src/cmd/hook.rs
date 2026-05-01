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

    // Cursor on Windows prepends a UTF-8 BOM; strip it before parsing.
    let raw = raw.trim_start_matches('\u{FEFF}');

    let preview: String = raw.chars().take(500).collect();
    crate::log::log("hook", &format!("stdin ({} bytes): {}", raw.len(), preview));

    if let Some(agent) = agent_override.as_deref() {
        crate::log::log("hook", &format!("--agent override: {}", agent));
    }

    let payload: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|e| {
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
    // Codex events pass through uninterpreted. Surface attribution
    // (Codex.app standalone vs Codex-via-VS-Code) is computed by the
    // tiles projection via temporal correlation with vscode-extension
    // focus events — see src/events/tiles/derive.rs.

    let agent_name = if project.is_empty() {
        agent_kind.slug().to_string()
    } else {
        format!("{}:{}", agent_kind.slug(), project)
    };

    // Locate where we are (terminal/IDE/browser). If `locate()` can't match
    // (common when the agent's hook subprocess isn't a child of the editor
    // process we know about — e.g. Cursor's AI agent), fall back to a
    // project-level URI synthesized from the payload for IDE-family agents.
    let terminal_uri = crate::workspace::locate().ok().map(|uri| {
        // For IDE URIs (code/cursor/windsurf) without a project: segment,
        // append the project name from the payload's cwd so that focus_by_pid
        // can look up the workspace folder via storage.json. The process CWD
        // at hook time is unreliable (.claude subdir), but the payload cwd is correct.
        let is_ide = uri.starts_with("workspace://code/")
            || uri.starts_with("workspace://cursor/")
            || uri.starts_with("workspace://windsurf/");
        if is_ide && !project.is_empty() && !uri.contains("/project:") {
            format!("{}/project:{}", uri, project)
        } else {
            uri
        }
    }).or_else(|| {
        // Cursor hook: synthesize a workspace-level URI when the hook's
        // parent chain doesn't reach the Cursor extension host. Cursor spawns
        // hooks from a sibling process of the extension host, so the ancestor
        // walk misses windowPid. Match the state file by workspace root instead
        // so we can include window:<pid> — without it cli_surface_token returns
        // None and tiles::derive() produces no tile.
        if agent_kind == crate::hooks::AgentKind::Cursor && !project.is_empty() {
            let workspace_root = payload
                .get("workspace_roots")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .or_else(|| payload.get("cwd").and_then(|v| v.as_str()));
            if let Some(pid) = workspace_root.and_then(cursor_window_pid_for_workspace) {
                return Some(format!(
                    "workspace://cursor/window:{}/project:{}",
                    pid, project
                ));
            }
            return Some(format!("workspace://cursor/project:{}", project));
        }
        None
    });

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

/// Find the windowPid of a Cursor window whose workspaceFolder matches `workspace_root`.
/// Reads all `~/.config/zestful/vscode/*.json` state files written by the extension.
fn cursor_window_pid_for_workspace(workspace_root: &str) -> Option<u32> {
    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)?
        .join(".config/zestful/vscode");
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        if val.get("appName").and_then(|v| v.as_str()) != Some("Cursor") {
            continue;
        }
        if val
            .get("workspaceFolder")
            .and_then(|v| v.as_str())
            == Some(workspace_root)
        {
            if let Some(pid) = val.get("windowPid").and_then(|v| v.as_u64()) {
                return Some(pid as u32);
            }
        }
    }
    None
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
