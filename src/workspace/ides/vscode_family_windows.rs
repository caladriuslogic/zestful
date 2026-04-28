//! Detection for VS Code and its forks on Windows.
//!
//! Strategy: check for a running process via `tasklist`, then read
//! `%APPDATA%\<App>\User\globalStorage\storage.json` which VS Code-family
//! editors keep updated with the currently-open window list under the
//! `windowsState` key. This is the most reliable detection method on Windows
//! as it does not depend on file-lock inspection or modification times.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::workspace::win32;

use crate::workspace::types::{IdeInstance, IdeProject};

struct AppSpec {
    process_name: &'static str,
    appdata_dir: &'static str,
    display: &'static str,
}

const APPS: &[AppSpec] = &[
    AppSpec {
        process_name: "Code.exe",
        appdata_dir: "Code",
        display: "Visual Studio Code",
    },
    AppSpec {
        process_name: "Cursor.exe",
        appdata_dir: "Cursor",
        display: "Cursor",
    },
    AppSpec {
        process_name: "Windsurf.exe",
        appdata_dir: "Windsurf",
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
        // Use the .cmd wrapper, not the main .exe — only the wrapper knows how
        // to IPC into the already-running instance for --reuse-window.
        match self {
            Family::VSCode => "code.cmd",
            Family::Cursor => "cursor.cmd",
            Family::Windsurf => "windsurf.cmd",
        }
    }
    fn url_scheme(self) -> &'static str {
        match self {
            Family::VSCode => "vscode",
            Family::Cursor => "cursor",
            Family::Windsurf => "windsurf",
        }
    }
    fn appdata_dir(self) -> &'static str {
        match self {
            Family::VSCode => "Code",
            Family::Cursor => "Cursor",
            Family::Windsurf => "Windsurf",
        }
    }
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
    let pid = win32::first_pid_by_exe(spec.process_name)?;
    let storage_json = appdata_dir()?
        .join(spec.appdata_dir)
        .join("User")
        .join("globalStorage")
        .join("storage.json");

    let projects = read_open_projects(&storage_json);

    Some(IdeInstance {
        app: spec.display.to_string(),
        pid: Some(pid),
        projects,
    })
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

    // lastActiveWindow comes first so it gets active=true below
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
    let decoded = decode_vscode_uri(uri);
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

/// Focus a VS Code window by process ID. The `window:<n>` segment in
/// `workspace://code/window:<n>` URIs carries the Node.js PID of the VS Code
/// extension host (or renderer), not a Win32 HWND. Walk up the parent-process
/// chain from that PID to the Code.exe ancestor that owns the visible window,
/// then raise it.
pub async fn focus_by_pid(window_id: &str) -> Result<()> {
    let pid: u32 = window_id
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid window pid: {}", window_id))?;
    tokio::task::spawn_blocking(move || {
        let hwnd = crate::workspace::win32::find_ancestor_window(pid, "Code.exe");
        if hwnd != 0 {
            crate::workspace::win32::raise_window(hwnd);
        }
    })
    .await?;
    Ok(())
}

/// Open a URI in the Zestful VS Code extension's URI handler (for terminal focus).
pub async fn focus_terminal(family: Family, terminal_id: &str) -> Result<()> {
    let url = format!(
        "{}://zestfuldev.zestful/focus?terminal={}",
        family.url_scheme(),
        terminal_id
    );
    tokio::task::spawn_blocking(move || {
        let _ = Command::new("cmd").args(["/c", "start", "", &url]).status();
    })
    .await?;
    Ok(())
}

pub async fn focus(family: Family, project_id: Option<&str>) -> Result<()> {
    let project_id_owned = project_id.map(String::from);
    tokio::task::spawn_blocking(move || focus_sync(family, project_id_owned.as_deref()))
        .await??;
    Ok(())
}

fn focus_sync(family: Family, project_id: Option<&str>) -> Result<()> {
    let cli = family.cli_name();
    if let Some(id) = project_id {
        if let Some(path) = lookup_project_path(family, id) {
            // Spawn without waiting — the .cmd wrapper sends IPC to the running
            // instance and exits on its own; blocking on it just adds latency.
            let _ = Command::new("cmd")
                .args(["/c", cli, "--reuse-window", &path])
                .spawn();
            return Ok(());
        }
    }
    let _ = Command::new("cmd").args(["/c", cli]).spawn();
    Ok(())
}

/// Look up the filesystem path for a currently-open project by name.
/// Reads from storage.json so only live windows are considered.
fn lookup_project_path(family: Family, project_name: &str) -> Option<String> {
    let storage_json = appdata_dir()?
        .join(family.appdata_dir())
        .join("User")
        .join("globalStorage")
        .join("storage.json");
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

/// Parse a VS Code `file://` URI into a local filesystem path.
/// Windows URIs look like `file:///C%3A/path` or `file:///C:/path`.
fn decode_vscode_uri(uri: &str) -> String {
    let local = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri);
    urlencoding_decode(local)
}

fn appdata_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

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
    use super::{decode_vscode_uri, urlencoding_decode};

    #[test]
    fn decodes_windows_drive_letter() {
        assert_eq!(
            decode_vscode_uri("file:///C%3A/Users/foo/project"),
            "C:/Users/foo/project"
        );
    }

    #[test]
    fn decodes_windows_uri_plain_colon() {
        assert_eq!(
            decode_vscode_uri("file:///C:/Users/foo/project"),
            "C:/Users/foo/project"
        );
    }

    #[test]
    fn decodes_spaces() {
        assert_eq!(urlencoding_decode("/foo%20bar"), "/foo bar");
    }
}
