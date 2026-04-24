//! Map agent-hook-stdin JSON payloads into a vector of event envelopes.
//!
//! Pure function: does the reading of host/os_user/device_id but no network
//! I/O. Implements the mapping table from
//! docs/superpowers/specs/2026-04-21-event-protocol-design.md §Mapping.

use crate::events::device;
use crate::events::env_capture;
use crate::events::envelope::{Context, Correlation, Envelope, Subapplication};
use crate::events::payload::{
    AgentNotified, Payload, PermissionRequested, SessionStarted, ToolCompleted, ToolInvoked,
    TurnCompleted, TurnPromptSubmitted,
};
use crate::events::preview::{sha256_hex, truncate_utf8_safe};
use crate::hooks::AgentKind;
use serde_json::Value;

const PROMPT_PREVIEW_MAX: usize = 1024;
const ARGS_PREVIEW_MAX: usize = 512;
const MESSAGE_MAX: usize = 1024;

/// Build envelopes from a single incoming hook payload. Returns `Vec` because
/// some hooks map to zero events (e.g. Cursor `beforeReadFile`) or to an
/// unknown event that produces a fallback.
///
/// `focus_uri` is the `workspace://…` URI already computed by the caller
/// (e.g. `cmd/hook.rs`), including any codex-editor or synthesised-project
/// fallback. Passing `None` produces a context with no `application` or
/// `application_instance` fields.
pub fn map_hook_payload(
    agent: AgentKind,
    payload: &Value,
    focus_uri: Option<String>,
) -> Vec<Envelope> {
    let event = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let payloads = match agent {
        AgentKind::ClaudeCode => claude_code(event, payload),
        AgentKind::CodexCli => codex(event, payload),
        AgentKind::Cursor => cursor(event, payload),
        _ => generic(event, payload),
    };

    // Wrap each Payload in a full Envelope with shared envelope fields.
    let host = hostname();
    let os_user = os_user();
    let device = device::device_id();
    let source = source_slug(agent);
    let source_pid = std::process::id();
    let ts = now_unix_ms();
    let correlation = correlation_from(payload);
    let context = context_from(agent, payload, focus_uri);

    payloads
        .into_iter()
        .enumerate()
        .map(|(i, p)| Envelope {
            id: ulid::Ulid::new().to_string(),
            schema: 1,
            ts,
            seq: i as u64,
            host: host.clone(),
            os_user: os_user.clone(),
            device_id: device.clone(),
            source: source.clone(),
            source_pid,
            type_: p.type_str().to_string(),
            correlation: correlation.clone(),
            context: context.clone(),
            payload: p.to_body_value(),
        })
        .collect()
}

// ---------- per-agent mapping tables ----------

fn claude_code(event: &str, payload: &Value) -> Vec<Payload> {
    match event {
        "UserPromptSubmit" => vec![turn_prompt_submitted(payload)],
        "Stop" | "SubagentStop" => vec![Payload::TurnCompleted(TurnCompleted::default())],
        "PreToolUse" => vec![tool_invoked(payload)],
        "PostToolUse" => vec![tool_completed(payload)],
        "Notification" => vec![agent_notified("notification", payload)],
        "Elicitation" => vec![agent_notified("elicitation", payload)],
        "PermissionRequest" => vec![permission_requested(payload)],
        _ => generic(event, payload),
    }
}

fn codex(event: &str, payload: &Value) -> Vec<Payload> {
    match event {
        "UserPromptSubmit" => vec![turn_prompt_submitted(payload)],
        "Stop" => vec![Payload::TurnCompleted(TurnCompleted::default())],
        "PreToolUse" => vec![tool_invoked(payload)],
        "PostToolUse" => vec![tool_completed(payload)],
        "SessionStart" => {
            let session_id = payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            vec![Payload::SessionStarted(SessionStarted {
                agent_session_id: session_id,
            })]
        }
        _ => generic(event, payload),
    }
}

fn cursor(event: &str, payload: &Value) -> Vec<Payload> {
    match event {
        "beforeSubmitPrompt" => vec![turn_prompt_submitted(payload)],
        "stop" => vec![Payload::TurnCompleted(TurnCompleted::default())],
        "beforeShellExecution" | "beforeMCPExecution" => vec![tool_invoked(payload)],
        // Skipped per spec — chatty, not worth the volume in v1.
        "beforeReadFile" | "afterFileEdit" => vec![],
        _ => generic(event, payload),
    }
}

/// Fallback for unknown agent/event combos. Emits a catch-all `agent.notified`
/// so the corpus never goes silent.
fn generic(event: &str, payload: &Value) -> Vec<Payload> {
    let msg = if event.is_empty() {
        "Agent activity".to_string()
    } else {
        format!("Event: {}", event)
    };
    let hook_msg = payload
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| truncate_utf8_safe(s, MESSAGE_MAX));
    vec![Payload::AgentNotified(AgentNotified {
        kind: "other".into(),
        message: hook_msg.or(Some(msg)),
    })]
}

// ---------- payload builders ----------

fn turn_prompt_submitted(payload: &Value) -> Payload {
    let prompt = payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Payload::TurnPromptSubmitted(TurnPromptSubmitted {
        prompt_preview: truncate_utf8_safe(prompt, PROMPT_PREVIEW_MAX),
        prompt_hash: sha256_hex(prompt),
        message: None,
    })
}

fn tool_invoked(payload: &Value) -> Payload {
    let tool_name = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("tool")
        .to_string();
    let args_str = payload
        .get("tool_input")
        .map(|v| v.to_string())
        .unwrap_or_default();
    Payload::ToolInvoked(ToolInvoked {
        tool_name,
        args_preview: truncate_utf8_safe(&args_str, ARGS_PREVIEW_MAX),
        args_hash: sha256_hex(&args_str),
        message: None,
    })
}

fn tool_completed(payload: &Value) -> Payload {
    let tool_name = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("tool")
        .to_string();
    let duration_ms = payload
        .get("duration_ms")
        .and_then(|v| v.as_u64());
    let success = payload
        .get("success")
        .and_then(|v| v.as_bool());
    let result_preview = payload
        .get("tool_response")
        .map(|v| v.to_string())
        .map(|s| truncate_utf8_safe(&s, ARGS_PREVIEW_MAX));
    Payload::ToolCompleted(ToolCompleted {
        tool_name,
        duration_ms,
        success,
        result_preview,
        message: None,
    })
}

fn agent_notified(kind: &str, payload: &Value) -> Payload {
    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| truncate_utf8_safe(s, MESSAGE_MAX));
    Payload::AgentNotified(AgentNotified {
        kind: kind.to_string(),
        message,
    })
}

fn permission_requested(payload: &Value) -> Payload {
    let kind = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .map(|_| "tool")
        .unwrap_or("other")
        .to_string();
    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| truncate_utf8_safe(s, MESSAGE_MAX))
        .unwrap_or_else(|| "Waiting for permission".into());
    Payload::PermissionRequested(PermissionRequested { kind, message })
}

// ---------- envelope field helpers ----------

fn hostname() -> String {
    if let Ok(name) = std::env::var("HOSTNAME") {
        if !name.is_empty() {
            return name;
        }
    }
    if let Ok(output) = std::process::Command::new("hostname").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "unknown".into()
}

fn os_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn source_slug(agent: AgentKind) -> String {
    agent.slug().to_string()
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn correlation_from(payload: &Value) -> Option<Correlation> {
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let turn_id = payload
        .get("turn_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let tool_use_id = payload
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    if session_id.is_none() && turn_id.is_none() && tool_use_id.is_none() {
        return None;
    }
    Some(Correlation {
        session_id,
        turn_id,
        tool_use_id,
        parent_id: None,
    })
}

/// Pick the most stable "workspace root" for this event.
///
/// Agent hook payloads often carry only the agent's *current* working
/// directory (which moves when the agent `cd`s or reads files in subdirs).
/// That's not what we want for `workspace_root` — we want the stable project
/// root the session was started in. Resolution order:
///
/// 1. `payload.workspace_roots[0]` — Cursor sends this directly in its hook.
/// 2. Agent-specific env var set when the session was started. See
///    `workspace_root_env_vars_for` for the per-agent mapping.
/// 3. `cwd` — last resort; matches legacy behavior when no better signal is
///    available.
fn resolve_workspace_root(
    agent: AgentKind,
    payload: &Value,
    cwd: Option<&str>,
) -> Option<String> {
    // 1. Payload-provided workspace roots (Cursor).
    if let Some(root) = payload
        .get("workspace_roots")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
    {
        return Some(root.to_string());
    }
    // 2. Agent-specific env var lookup. First non-empty value wins.
    for env_var in workspace_root_env_vars_for(agent) {
        if let Ok(v) = std::env::var(env_var) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    // 3. cwd fallback.
    cwd.map(String::from)
}

/// Per-agent list of env vars that, when set, identify the stable project
/// root the agent was started against. Checked in order; first non-empty
/// value wins.
///
/// Verified conventions:
/// - `CLAUDE_PROJECT_DIR` — Claude Code documents this for hook subprocesses.
///   https://docs.anthropic.com/claude-code/hooks
///
/// Unverified / pending agents (currently fall through to `cwd`; add their
/// env var here when the upstream convention is confirmed):
/// - CodexCli: no documented project-dir env var at time of writing.
///   Codex emits `cwd` in the hook payload; that's our only signal.
/// - CopilotCli, Cline, Aider, GeminiCli: no known project-dir env var.
/// - Cursor: provides `workspace_roots[]` directly in the payload, handled
///   above — no env var lookup needed.
/// - Generic: unknown agent, nothing to look up.
fn workspace_root_env_vars_for(agent: AgentKind) -> &'static [&'static str] {
    match agent {
        AgentKind::ClaudeCode => &["CLAUDE_PROJECT_DIR"],
        AgentKind::CodexCli
        | AgentKind::CopilotCli
        | AgentKind::Cline
        | AgentKind::Aider
        | AgentKind::Cursor
        | AgentKind::GeminiCli
        | AgentKind::Generic => &[],
    }
}

fn context_from(
    agent: AgentKind,
    payload: &Value,
    focus_uri: Option<String>,
) -> Option<Context> {
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(String::from);
    let workspace_root = resolve_workspace_root(agent, payload, cwd.as_deref());
    let project = workspace_root
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|s| s.to_string_lossy().into_owned());
    let shell = std::env::var("SHELL").ok().and_then(|s| {
        std::path::Path::new(&s)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
    });
    let sub = tmux_subapplication();

    // focus_uri passed in by caller — the hook has already computed it
    // (including any codex_editor fallback routing). Don't re-locate.

    // application + application_instance: parsed out of the focus_uri.
    // Examples:
    //   workspace://iterm2/window:1/tab:2          → app="iterm2", instance="window:1/tab:2"
    //   workspace://vscode/window:80836/project:X  → app="vscode",  instance="window:80836"
    //   workspace://codex                           → app="codex",   instance=None
    let (application, application_instance) = focus_uri
        .as_deref()
        .and_then(crate::workspace::uri::parse_terminal_uri)
        .map(|p| {
            let mut parts = Vec::with_capacity(2);
            if let Some(w) = p.window_id.as_deref() {
                parts.push(format!("window:{}", w));
            }
            if let Some(t) = p.tab_id.as_deref() {
                parts.push(format!("tab:{}", t));
            }
            let instance = if parts.is_empty() { None } else { Some(parts.join("/")) };
            (Some(p.app), instance)
        })
        .unwrap_or((None, None));

    let ctx = Context {
        agent: Some(agent.slug().to_string()),
        model: payload
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from),
        application,
        application_instance,
        focus_uri,
        shell,
        subapplication: sub,
        cwd,
        workspace_root,
        project,
        env_vars_observed: env_capture::capture(),
        ..Default::default()
    };
    Some(ctx)
}

fn tmux_subapplication() -> Option<Subapplication> {
    // $TMUX is set inside tmux to "<socket>,<pid>,<session_id>". If absent,
    // the shell isn't running in tmux.
    let tmux_var = std::env::var("TMUX").ok()?;
    let session_id = tmux_var.split(',').nth(2).map(String::from);
    Some(Subapplication {
        kind: "tmux".into(),
        session: session_id,
        window: std::env::var("TMUX_WINDOW").ok(),
        pane: std::env::var("TMUX_PANE").ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Build a minimal payload JSON for tests.
    fn payload_with(event: &str, extras: serde_json::Map<String, Value>) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("hook_event_name".into(), Value::String(event.into()));
        for (k, v) in extras {
            obj.insert(k, v);
        }
        Value::Object(obj)
    }

    #[test]
    fn claude_code_user_prompt_submit_maps_to_turn_prompt_submitted() {
        let p = payload_with(
            "UserPromptSubmit",
            serde_json::Map::from_iter([(
                "prompt".into(),
                json!("hello world"),
            )]),
        );
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "turn.prompt_submitted");
        assert_eq!(envs[0].payload["prompt_preview"], "hello world");
        assert_eq!(envs[0].payload["prompt_hash"].as_str().unwrap().len(), 64);
        assert_eq!(envs[0].source, "claude-code");
        assert_eq!(envs[0].seq, 0);
    }

    #[test]
    fn claude_code_stop_maps_to_turn_completed() {
        let p = payload_with("Stop", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "turn.completed");
    }

    #[test]
    fn claude_code_pre_tool_use_maps_to_tool_invoked_with_hash() {
        let p = payload_with(
            "PreToolUse",
            serde_json::Map::from_iter([
                ("tool_name".into(), json!("Bash")),
                ("tool_input".into(), json!({"command": "ls -la"})),
            ]),
        );
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "tool.invoked");
        assert_eq!(envs[0].payload["tool_name"], "Bash");
        let args_hash = envs[0].payload["args_hash"].as_str().unwrap();
        assert_eq!(args_hash.len(), 64);
    }

    #[test]
    fn claude_code_notification_carries_message() {
        let p = payload_with(
            "Notification",
            serde_json::Map::from_iter([(
                "message".into(),
                json!("Needs your attention"),
            )]),
        );
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "agent.notified");
        assert_eq!(envs[0].payload["kind"], "notification");
        assert_eq!(envs[0].payload["message"], "Needs your attention");
    }

    #[test]
    fn claude_code_permission_request_uses_message_from_payload() {
        let p = payload_with(
            "PermissionRequest",
            serde_json::Map::from_iter([
                ("tool_name".into(), json!("Write")),
                ("message".into(), json!("Write to /etc/passwd?")),
            ]),
        );
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "permission.requested");
        assert_eq!(envs[0].payload["kind"], "tool");
        assert_eq!(envs[0].payload["message"], "Write to /etc/passwd?");
    }

    #[test]
    fn cursor_stop_maps_to_turn_completed() {
        let p = payload_with("stop", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::Cursor, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "turn.completed");
    }

    #[test]
    fn cursor_before_read_file_produces_no_events() {
        let p = payload_with("beforeReadFile", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::Cursor, &p, None);
        assert!(envs.is_empty());
    }

    #[test]
    fn cursor_after_file_edit_produces_no_events() {
        let p = payload_with("afterFileEdit", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::Cursor, &p, None);
        assert!(envs.is_empty());
    }

    #[test]
    fn codex_session_start_maps_with_id() {
        let p = payload_with(
            "SessionStart",
            serde_json::Map::from_iter([(
                "session_id".into(),
                json!("s_codex_42"),
            )]),
        );
        let envs = map_hook_payload(AgentKind::CodexCli, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "session.started");
        assert_eq!(envs[0].payload["agent_session_id"], "s_codex_42");
    }

    #[test]
    fn unknown_event_falls_back_to_agent_notified() {
        let p = payload_with("UnheardOf", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].type_, "agent.notified");
        assert_eq!(envs[0].payload["kind"], "other");
    }

    #[test]
    fn envelope_has_required_fields() {
        let p = payload_with("Stop", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        let env = &envs[0];
        assert_eq!(env.schema, 1);
        assert!(env.ts > 0);
        assert_eq!(env.id.len(), 26);
        assert!(!env.host.is_empty());
        assert!(!env.os_user.is_empty());
        assert!(!env.device_id.is_empty());
        assert_eq!(env.source, "claude-code");
        assert_eq!(env.source_pid, std::process::id());
    }

    #[test]
    fn correlation_populated_when_session_id_present() {
        let p = payload_with(
            "Stop",
            serde_json::Map::from_iter([(
                "session_id".into(),
                json!("sess_abc"),
            )]),
        );
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        let correlation = envs[0].correlation.as_ref().expect("correlation should be set");
        assert_eq!(correlation.session_id.as_deref(), Some("sess_abc"));
    }

    #[test]
    fn correlation_absent_when_no_ids() {
        let p = payload_with("Stop", serde_json::Map::new());
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        assert!(envs[0].correlation.is_none());
    }

    #[test]
    fn prompt_preview_truncated_to_budget() {
        let long_prompt = "x".repeat(2000);
        let p = payload_with(
            "UserPromptSubmit",
            serde_json::Map::from_iter([(
                "prompt".into(),
                json!(long_prompt),
            )]),
        );
        let envs = map_hook_payload(AgentKind::ClaudeCode, &p, None);
        let preview = envs[0].payload["prompt_preview"].as_str().unwrap();
        assert_eq!(preview.len(), 1024);
        // Hash is of the FULL string, not the preview.
        let expected_hash = sha256_hex(&long_prompt);
        assert_eq!(envs[0].payload["prompt_hash"], expected_hash);
    }

    #[test]
    fn mapped_envelope_passes_spec_validator_rules() {
        // Round-trip: map_hook_payload output → serde_json::to_value →
        // the same envelope shape rules that cmd::daemon::validate_envelope
        // enforces. Catches field-name drift between emitter and daemon
        // without a cross-module dep.
        let payload = json!({
            "hook_event_name": "Stop",
            "session_id": "sess_drift",
        });
        let envs = map_hook_payload(AgentKind::ClaudeCode, &payload, None);
        assert_eq!(envs.len(), 1);
        let v = serde_json::to_value(&envs[0]).unwrap();
        let obj = v.as_object().expect("envelope is object");

        // Every field listed in the spec §Envelope field rules table as
        // required must be present on the emitter-produced JSON.
        for required in [
            "id", "schema", "ts", "seq", "host", "os_user",
            "device_id", "source", "source_pid", "type",
        ] {
            assert!(
                obj.contains_key(required),
                "emitter output missing required field: {}",
                required
            );
        }

        // Envelope-level shape invariants the daemon validator checks.
        assert_eq!(obj["schema"].as_u64(), Some(1));
        assert_eq!(obj["id"].as_str().map(|s| s.len()), Some(26));
        assert!(obj["type"].is_string());
    }

    // Tests below depend on `CLAUDE_PROJECT_DIR` env var state and so must
    // run single-threaded (already enforced crate-wide via --test-threads=1).
    // Tests mutating CLAUDE_PROJECT_DIR rely on --test-threads=1 for isolation.
    // SAFETY on each set_var/remove_var: single-threaded test mode means no
    // other thread is reading env vars during the mutation.

    #[test]
    fn resolve_workspace_root_prefers_env_var_for_claude_code() {
        let prior = std::env::var("CLAUDE_PROJECT_DIR").ok();
        unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", "/Users/test/project-root"); }
        let payload = json!({ "hook_event_name": "PreToolUse", "cwd": "/Users/test/project-root/sub/dir" });
        let root = resolve_workspace_root(
            AgentKind::ClaudeCode,
            &payload,
            Some("/Users/test/project-root/sub/dir"),
        );
        // Restore env before asserting so a failure doesn't leak state.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("CLAUDE_PROJECT_DIR", v),
                None => std::env::remove_var("CLAUDE_PROJECT_DIR"),
            }
        }
        assert_eq!(root.as_deref(), Some("/Users/test/project-root"));
    }

    #[test]
    fn resolve_workspace_root_falls_back_to_cwd_when_env_unset() {
        let prior = std::env::var("CLAUDE_PROJECT_DIR").ok();
        unsafe { std::env::remove_var("CLAUDE_PROJECT_DIR"); }
        let payload = json!({ "hook_event_name": "PreToolUse", "cwd": "/tmp/nowhere" });
        let root = resolve_workspace_root(
            AgentKind::ClaudeCode,
            &payload,
            Some("/tmp/nowhere"),
        );
        if let Some(v) = prior {
            unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", v); }
        }
        assert_eq!(root.as_deref(), Some("/tmp/nowhere"));
    }

    #[test]
    fn resolve_workspace_root_prefers_payload_roots_over_env() {
        let prior = std::env::var("CLAUDE_PROJECT_DIR").ok();
        unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", "/should-not-win"); }
        let payload = json!({
            "hook_event_name": "beforeSubmitPrompt",
            "workspace_roots": ["/cursor/says/this"],
            "cwd": "/whatever",
        });
        let root = resolve_workspace_root(AgentKind::Cursor, &payload, Some("/whatever"));
        unsafe {
            match prior {
                Some(v) => std::env::set_var("CLAUDE_PROJECT_DIR", v),
                None => std::env::remove_var("CLAUDE_PROJECT_DIR"),
            }
        }
        assert_eq!(root.as_deref(), Some("/cursor/says/this"));
    }

    #[test]
    fn resolve_workspace_root_ignores_env_for_non_claude_agents() {
        let prior = std::env::var("CLAUDE_PROJECT_DIR").ok();
        unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", "/claude-project-root"); }
        let payload = json!({ "hook_event_name": "SessionStart", "cwd": "/codex/cwd" });
        let root = resolve_workspace_root(AgentKind::CodexCli, &payload, Some("/codex/cwd"));
        unsafe {
            match prior {
                Some(v) => std::env::set_var("CLAUDE_PROJECT_DIR", v),
                None => std::env::remove_var("CLAUDE_PROJECT_DIR"),
            }
        }
        assert_eq!(root.as_deref(), Some("/codex/cwd"));
    }

    #[test]
    fn workspace_root_env_vars_mapping_is_stable() {
        // Claude Code is the only agent with a documented project-dir env var
        // today; other agents either provide the root via payload (Cursor) or
        // have no known env var (everyone else). This test pins that policy.
        assert_eq!(workspace_root_env_vars_for(AgentKind::ClaudeCode), &["CLAUDE_PROJECT_DIR"]);
        assert!(workspace_root_env_vars_for(AgentKind::Cursor).is_empty());
        assert!(workspace_root_env_vars_for(AgentKind::CodexCli).is_empty());
        assert!(workspace_root_env_vars_for(AgentKind::CopilotCli).is_empty());
        assert!(workspace_root_env_vars_for(AgentKind::Cline).is_empty());
        assert!(workspace_root_env_vars_for(AgentKind::Aider).is_empty());
        assert!(workspace_root_env_vars_for(AgentKind::GeminiCli).is_empty());
        assert!(workspace_root_env_vars_for(AgentKind::Generic).is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn context_includes_env_vars_observed_when_set() {
        let prior = std::env::var("CLAUDE_PROJECT_DIR").ok();
        unsafe { std::env::set_var("CLAUDE_PROJECT_DIR", "/x/env-vars-test"); }

        let payload = serde_json::json!({ "cwd": "/whatever" });
        let ctx = context_from(AgentKind::ClaudeCode, &payload, None).expect("expected Some(ctx)");
        let observed_value = ctx
            .env_vars_observed
            .as_ref()
            .and_then(|m| m.get("CLAUDE_PROJECT_DIR"))
            .map(String::as_str)
            .map(String::from);

        // Restore before asserting so a failed assert doesn't leak env state.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("CLAUDE_PROJECT_DIR", v),
                None    => std::env::remove_var("CLAUDE_PROJECT_DIR"),
            }
        }

        assert_eq!(observed_value.as_deref(), Some("/x/env-vars-test"));
    }

    #[test]
    fn context_from_uses_passed_focus_uri_for_application_instance() {
        // When the hook passes a URI like workspace://vscode/window:80836/project:zestful,
        // context_from should parse it and set application_instance to "window:80836".
        let payload = serde_json::json!({ "cwd": "/Users/x/Development/zestful" });
        let focus_uri = Some("workspace://vscode/window:80836/project:zestful".to_string());
        let ctx = context_from(AgentKind::CodexCli, &payload, focus_uri.clone())
            .expect("expected Some(ctx)");
        assert_eq!(ctx.focus_uri, focus_uri);
        assert_eq!(ctx.application.as_deref(), Some("vscode"));
        assert_eq!(ctx.application_instance.as_deref(), Some("window:80836"));
    }

    #[test]
    fn context_from_with_none_focus_uri_leaves_application_fields_none() {
        let payload = serde_json::json!({ "cwd": "/x" });
        let ctx = context_from(AgentKind::ClaudeCode, &payload, None)
            .expect("expected Some(ctx)");
        assert_eq!(ctx.focus_uri, None);
        assert_eq!(ctx.application, None);
        assert_eq!(ctx.application_instance, None);
    }

    #[test]
    fn context_from_with_window_and_tab_joins_instance_segments() {
        let payload = serde_json::json!({ "cwd": "/x" });
        let focus_uri = Some("workspace://iterm2/window:1/tab:2".to_string());
        let ctx = context_from(AgentKind::ClaudeCode, &payload, focus_uri.clone())
            .expect("expected Some(ctx)");
        assert_eq!(ctx.focus_uri, focus_uri);
        assert_eq!(ctx.application.as_deref(), Some("iTerm2"));
        assert_eq!(ctx.application_instance.as_deref(), Some("window:1/tab:2"));
    }

    #[test]
    fn context_from_with_app_only_uri_leaves_instance_none() {
        // The Codex-desktop-no-editor case: workspace://codex has no window/tab.
        // app="codex", application_instance=None.
        let payload = serde_json::json!({ "cwd": "/x" });
        let focus_uri = Some("workspace://codex".to_string());
        let ctx = context_from(AgentKind::CodexCli, &payload, focus_uri.clone())
            .expect("expected Some(ctx)");
        assert_eq!(ctx.focus_uri, focus_uri);
        assert_eq!(ctx.application.as_deref(), Some("codex"));
        assert_eq!(ctx.application_instance, None);
    }
}
