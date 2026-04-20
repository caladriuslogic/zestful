//! Detection for VS Code and its forks (Cursor, Windsurf, etc.).
//!
//! Strategy: each open workspace window keeps a `state.vscdb` file open under
//! `~/Library/Application Support/<App>/User/workspaceStorage/<hash>/`. The
//! sibling `workspace.json` records the folder URI. We use `lsof` against
//! the running app's PID to find which workspace storage dirs are *currently*
//! open, then read each `workspace.json` to extract the project path.
//!
//! This avoids needing Accessibility / System Events permission.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::workspace::types::{IdeInstance, IdeProject};

struct AppSpec {
    process_name: &'static str,
    support_dir: &'static str,  // relative to ~/Library/Application Support/
    display: &'static str,
}

const APPS: &[AppSpec] = &[
    AppSpec { process_name: "Code",     support_dir: "Code",     display: "Visual Studio Code" },
    AppSpec { process_name: "Cursor",   support_dir: "Cursor",   display: "Cursor" },
    AppSpec { process_name: "Windsurf", support_dir: "Windsurf", display: "Windsurf" },
];

/// Which VS Code-family editor to target for focus.
#[derive(Copy, Clone, Debug)]
pub enum Family {
    VSCode,
    Cursor,
    Windsurf,
}

impl Family {
    fn cli_name(self) -> &'static str {
        match self {
            Family::VSCode => "code",
            Family::Cursor => "cursor",
            Family::Windsurf => "windsurf",
        }
    }
    /// URL scheme registered by each editor's bundle. The Zestful extension
    /// becomes reachable at `<scheme>://zestfuldev.zestful/...` in whichever
    /// host it's installed.
    fn url_scheme(self) -> &'static str {
        match self {
            Family::VSCode => "vscode",
            Family::Cursor => "cursor",
            Family::Windsurf => "windsurf",
        }
    }
    fn app_bundle_name(self) -> &'static str {
        match self {
            Family::VSCode => "Visual Studio Code",
            Family::Cursor => "Cursor",
            Family::Windsurf => "Windsurf",
        }
    }
    fn support_dir(self) -> &'static str {
        match self {
            Family::VSCode => "Code",
            Family::Cursor => "Cursor",
            Family::Windsurf => "Windsurf",
        }
    }
    fn process_name(self) -> &'static str {
        match self {
            Family::VSCode => "Code",
            Family::Cursor => "Cursor",
            Family::Windsurf => "Windsurf",
        }
    }
}

/// Focus a specific integrated terminal in a VS Code-family editor by
/// opening the URI handler the Zestful VS Code extension registers. The
/// extension finds the terminal across all open windows and calls show().
pub async fn focus_terminal(family: Family, terminal_id: &str) -> Result<()> {
    let url = format!(
        "{}://zestfuldev.zestful/focus?terminal={}",
        family.url_scheme(),
        terminal_id
    );
    // Bring the host editor to the front first so the URL handler lands on
    // an actually-frontmost window of the right app.
    let app_name = family.app_bundle_name().to_string();
    tokio::task::spawn_blocking(move || {
        crate::workspace::uri::activate_app_sync(&app_name);
        let _ = std::process::Command::new("/usr/bin/open")
            .arg(&url)
            .status();
    })
    .await?;
    Ok(())
}

/// Focus a VS Code-family project window. If `project_id` is given, resolve
/// its path from the editor's workspaceStorage and reopen (the editor will
/// promote the matching window to the front); if not, just activate the app.
pub async fn focus(family: Family, project_id: Option<&str>) -> Result<()> {
    let project_id_owned = project_id.map(String::from);
    let family_move = family;
    tokio::task::spawn_blocking(move || {
        focus_sync(family_move, project_id_owned.as_deref())
    })
    .await??;
    Ok(())
}

fn focus_sync(family: Family, project_id: Option<&str>) -> Result<()> {
    if let Some(id) = project_id {
        if let Some(path) = lookup_project_path(family, id) {
            let cli = find_cli(family);
            // Try the CLI first (handles window reuse cleanly); fall back to
            // `open -a <App> <path>` if the CLI isn't installed on PATH.
            if let Some(cli_path) = cli {
                let ok = Command::new(&cli_path)
                    .args(["--reuse-window", &path])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok { return Ok(()); }
            }
            let _ = Command::new("/usr/bin/open")
                .args(["-a", family.app_bundle_name(), &path])
                .status();
            return Ok(());
        }
    }
    // No project id (or unresolved): just activate the app.
    crate::workspace::uri::activate_app_sync(family.app_bundle_name());
    Ok(())
}

/// Search well-known locations for the family's CLI binary.
fn find_cli(family: Family) -> Option<std::path::PathBuf> {
    let cli_name = family.cli_name();
    let candidates: &[&str] = match family {
        Family::VSCode => &[
            "/usr/local/bin/code",
            "/opt/homebrew/bin/code",
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
        ],
        Family::Cursor => &[
            "/usr/local/bin/cursor",
            "/opt/homebrew/bin/cursor",
            "/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
        ],
        Family::Windsurf => &[
            "/usr/local/bin/windsurf",
            "/opt/homebrew/bin/windsurf",
        ],
    };
    for path in candidates {
        if std::path::Path::new(path).exists() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    // Fallback: rely on PATH via `which`
    Command::new("/usr/bin/which")
        .arg(cli_name)
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(std::path::PathBuf::from(s)) }
        })
}

/// Look up the workspace folder path for a given project name by scanning
/// the editor's workspaceStorage directories.
fn lookup_project_path(family: Family, project_name: &str) -> Option<String> {
    let home = home_dir()?;
    let storage = home
        .join("Library/Application Support")
        .join(family.support_dir())
        .join("User/workspaceStorage");
    let entries = fs::read_dir(&storage).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ws_json = path.join("workspace.json");
        let Ok(contents) = fs::read_to_string(&ws_json) else { continue };
        let Ok(parsed) = serde_json::from_str::<WorkspaceFile>(&contents) else { continue };
        let Some(uri) = parsed.folder.or(parsed.workspace) else { continue };
        let local = uri.strip_prefix("file://").unwrap_or(&uri);
        let decoded = urlencoding_decode(local);
        let name = PathBuf::from(&decoded)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == project_name {
            return Some(decoded);
        }
    }
    // Suppress unused-variable warning on non-macOS
    let _ = family.process_name();
    None
}

pub fn detect_all() -> Result<Vec<IdeInstance>> {
    let mut out = Vec::new();
    for spec in APPS {
        if let Some(instance) = detect_one(spec) {
            out.push(instance);
        }
    }
    Ok(out)
}

fn detect_one(spec: &AppSpec) -> Option<IdeInstance> {
    let pid = pgrep_first(spec.process_name)?;
    let storage_root = home_dir()?
        .join("Library/Application Support")
        .join(spec.support_dir)
        .join("User/workspaceStorage");

    let open_dirs = lsof_workspace_dirs(pid, &storage_root);
    let projects: Vec<IdeProject> = open_dirs
        .iter()
        .filter_map(|dir| read_workspace_project(dir))
        .collect();

    Some(IdeInstance {
        app: spec.display.to_string(),
        pid: Some(pid),
        projects,
    })
}

/// Use `lsof` to find every workspace storage directory currently held open
/// by the given app PID.
fn lsof_workspace_dirs(pid: u32, storage_root: &PathBuf) -> Vec<PathBuf> {
    let output = Command::new("lsof").args(["-p", &pid.to_string()]).output();
    let stdout = match output {
        Ok(o) if o.status.success() || !o.stdout.is_empty() => {
            String::from_utf8_lossy(&o.stdout).into_owned()
        }
        _ => return vec![],
    };

    let prefix = storage_root.to_string_lossy().to_string() + "/";
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for line in stdout.lines() {
        if let Some(idx) = line.find(&prefix) {
            let rest = &line[idx + prefix.len()..];
            // Hash directory is the segment up to the next "/"
            let hash = rest.split('/').next().unwrap_or("");
            if !hash.is_empty() && seen.insert(hash.to_string()) {
                dirs.push(storage_root.join(hash));
            }
        }
    }
    dirs
}

#[derive(Deserialize)]
struct WorkspaceFile {
    folder: Option<String>,
    workspace: Option<String>,
}

/// Read `<dir>/workspace.json` and return an IdeProject for its folder/workspace path.
fn read_workspace_project(dir: &PathBuf) -> Option<IdeProject> {
    let path = dir.join("workspace.json");
    let contents = fs::read_to_string(&path).ok()?;
    let parsed: WorkspaceFile = serde_json::from_str(&contents).ok()?;
    let uri = parsed.folder.or(parsed.workspace)?;
    let local = uri.strip_prefix("file://").unwrap_or(&uri);
    let decoded = urlencoding_decode(local);
    let name = PathBuf::from(&decoded)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    Some(IdeProject {
        name,
        uri: None,
        path: decoded,
        active: false,  // can't tell without UI scripting
    })
}

fn pgrep_first(name: &str) -> Option<u32> {
    let output = Command::new("pgrep").args(["-x", name]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .next()?
        .parse()
        .ok()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Minimal percent-decode for file:// URIs (just spaces and common chars).
fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::urlencoding_decode;

    #[test]
    fn decodes_spaces() {
        assert_eq!(urlencoding_decode("/foo%20bar"), "/foo bar");
    }

    #[test]
    fn passes_through_plain() {
        assert_eq!(urlencoding_decode("/foo/bar"), "/foo/bar");
    }
}
