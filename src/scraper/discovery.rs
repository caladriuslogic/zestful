//! Per-OS path discovery for agent transcript roots. Resolves the
//! actual directories the watcher will register, applying env-var
//! overrides and any extras from settings.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Root {
    pub agent: &'static str,
    pub path: PathBuf,
}

/// Resolve all roots to watch. Order:
///   1. Per-agent env override (ZESTFUL_CLAUDE_ROOT, ZESTFUL_CODEX_ROOT)
///   2. Per-OS default
///   3. settings.scraper.extra_roots
///
/// Roots may not exist on disk yet. Caller (watcher) handles deferred
/// registration. This function does not touch the filesystem.
pub fn resolve_roots() -> Vec<Root> {
    let mut roots = Vec::new();
    roots.push(Root {
        agent: "claude-code",
        path: env_or_default("ZESTFUL_CLAUDE_ROOT", default_claude_root()),
    });
    roots.push(Root {
        agent: "codex",
        path: env_or_default("ZESTFUL_CODEX_ROOT", default_codex_root()),
    });
    roots.extend(extra_roots_from_settings());
    roots
}

fn env_or_default(key: &str, default: PathBuf) -> PathBuf {
    std::env::var(key).map(PathBuf::from).unwrap_or(default)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn default_claude_root() -> PathBuf {
    home().join(".claude/projects")
}
#[cfg(target_os = "windows")]
fn default_claude_root() -> PathBuf {
    home().join(".claude").join("projects")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn default_codex_root() -> PathBuf {
    home().join(".codex/sessions")
}
#[cfg(target_os = "windows")]
fn default_codex_root() -> PathBuf {
    home().join(".codex").join("sessions")
}

fn home() -> PathBuf {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        std::env::var("HOME").map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\"))
    }
}

fn extra_roots_from_settings() -> Vec<Root> {
    crate::config::scraper_extra_roots()
        .into_iter()
        .filter_map(|(agent, path)| {
            // Only accept agents we have parsers for. Unknown agents
            // would never be parsed anyway; surface as silent ignore.
            let agent_static: &'static str = match agent.as_str() {
                "claude-code" => "claude-code",
                "codex" => "codex",
                _ => return None,
            };
            Some(Root { agent: agent_static, path: PathBuf::from(path) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn defaults_present_when_no_env_or_settings() {
        std::env::remove_var("ZESTFUL_CLAUDE_ROOT");
        std::env::remove_var("ZESTFUL_CODEX_ROOT");
        let roots = resolve_roots();
        assert!(roots.iter().any(|r| r.agent == "claude-code"));
        assert!(roots.iter().any(|r| r.agent == "codex"));
    }

    #[test]
    #[serial]
    fn env_override_wins_for_claude() {
        std::env::set_var("ZESTFUL_CLAUDE_ROOT", "/tmp/custom-claude");
        let roots = resolve_roots();
        let claude = roots.iter().find(|r| r.agent == "claude-code").unwrap();
        assert_eq!(claude.path, PathBuf::from("/tmp/custom-claude"));
        std::env::remove_var("ZESTFUL_CLAUDE_ROOT");
    }

    #[test]
    #[serial]
    fn env_override_wins_for_codex() {
        std::env::set_var("ZESTFUL_CODEX_ROOT", "/tmp/custom-codex");
        let roots = resolve_roots();
        let codex = roots.iter().find(|r| r.agent == "codex").unwrap();
        assert_eq!(codex.path, PathBuf::from("/tmp/custom-codex"));
        std::env::remove_var("ZESTFUL_CODEX_ROOT");
    }
}
