//! Detection for VS Code and its forks (Cursor, Windsurf, etc.).
//!
//! Strategy: confirm the editor is running via `ps`, then read
//! `~/Library/Application Support/<App>/User/globalStorage/storage.json`
//! which VS Code-family editors keep updated with the open window list under
//! the `windowsState` key. This mirrors the Windows detection approach.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::workspace::types::{IdeInstance, IdeProject};

struct AppSpec {
    process_name: &'static str,
    bundle_name: &'static str, // macOS .app bundle name
    support_dir: &'static str, // relative to ~/Library/Application Support/
    display: &'static str,
}

const APPS: &[AppSpec] = &[
    AppSpec {
        process_name: "Code",
        bundle_name: "Visual Studio Code",
        support_dir: "Code",
        display: "Visual Studio Code",
    },
    AppSpec {
        process_name: "Cursor",
        bundle_name: "Cursor",
        support_dir: "Cursor",
        display: "Cursor",
    },
    AppSpec {
        process_name: "Windsurf",
        bundle_name: "Windsurf",
        support_dir: "Windsurf",
        display: "Windsurf",
    },
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
/// its path from the editor's storage.json and reopen (the editor will
/// promote the matching window to the front); if not, just activate the app.
pub async fn focus(family: Family, project_id: Option<&str>) -> Result<()> {
    let project_id_owned = project_id.map(String::from);
    let family_move = family;
    tokio::task::spawn_blocking(move || focus_sync(family_move, project_id_owned.as_deref()))
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
                if ok {
                    return Ok(());
                }
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
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
            "/usr/local/bin/code",
            "/opt/homebrew/bin/code",
        ],
        Family::Cursor => &[
            "/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
            "/usr/local/bin/cursor",
            "/opt/homebrew/bin/cursor",
        ],
        Family::Windsurf => &["/usr/local/bin/windsurf", "/opt/homebrew/bin/windsurf"],
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
            if s.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(s))
            }
        })
}

/// Look up the workspace folder path for a given project name by scanning
/// storage.json for a matching open window.
fn lookup_project_path(family: Family, project_name: &str) -> Option<String> {
    let storage_json = home_dir()?
        .join("Library/Application Support")
        .join(family.support_dir())
        .join("User/globalStorage/storage.json");
    let contents = fs::read_to_string(&storage_json).ok()?;
    let root: Value = serde_json::from_str(&contents).ok()?;
    let ws = root.get("windowsState")?;

    let last = ws.get("lastActiveWindow").into_iter();
    let opened = ws
        .get("openedWindows")
        .and_then(|w| w.as_array())
        .map(|a| a.iter())
        .into_iter()
        .flatten();

    for win in last.chain(opened) {
        if let Some(path) = window_folder(win) {
            let name = PathBuf::from(&path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name == project_name {
                return Some(path);
            }
        }
    }
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
    let pid = ps_first_pid(spec.bundle_name, spec.process_name)?;
    let storage_json = home_dir()?
        .join("Library/Application Support")
        .join(spec.support_dir)
        .join("User/globalStorage/storage.json");
    let projects = read_open_projects(&storage_json);
    Some(IdeInstance {
        app: spec.display.to_string(),
        pid: Some(pid),
        projects,
    })
}

/// Find the PID of a VS Code-family editor by matching its `.app` bundle path
/// in `ps` output. Works on macOS versions where `pgrep` cannot enumerate the
/// main Electron process due to privacy restrictions.
fn ps_first_pid(bundle_name: &str, binary_name: &str) -> Option<u32> {
    let suffix = format!("{}.app/Contents/MacOS/{}", bundle_name, binary_name);
    let output = Command::new("ps").args(["-xo", "pid=,command="]).output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(&suffix) {
            return trimmed.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Parse the currently-open window folders from VS Code's `storage.json`.
///
/// The file contains a `windowsState` object with `lastActiveWindow` and
/// `openedWindows` entries, each optionally carrying a `folder` URI.
fn read_open_projects(storage_json: &PathBuf) -> Vec<IdeProject> {
    let contents = match fs::read_to_string(storage_json) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let root: Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let Some(ws) = root.get("windowsState") else {
        return vec![];
    };

    let mut projects: Vec<IdeProject> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let last_active_folder = ws
        .get("lastActiveWindow")
        .and_then(|w| window_folder(w));

    let mut add = |folder: String, active: bool| {
        if seen.insert(folder.clone()) {
            let name = PathBuf::from(&folder)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if !name.is_empty() {
                projects.push(IdeProject {
                    name,
                    uri: None,
                    path: folder,
                    active,
                });
            }
        }
    };

    if let Some(f) = last_active_folder.clone() {
        add(f, true);
    }
    if let Some(opened) = ws.get("openedWindows").and_then(|w| w.as_array()) {
        for win in opened {
            if let Some(f) = window_folder(win) {
                let is_active = last_active_folder.as_deref() == Some(&f);
                add(f, is_active);
            }
        }
    }

    projects
}

fn window_folder(win: &Value) -> Option<String> {
    let uri = win.get("folder")?.as_str()?;
    let local = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = urlencoding_decode(local);
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
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
