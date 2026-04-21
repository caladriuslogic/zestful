//! Detection for VS Code and its forks on Windows.
//!
//! Strategy: check for a running process via `tasklist`, then identify
//! currently open workspaces by comparing `state.vscdb` modification time
//! against the process start time. VS Code writes to this file when a
//! workspace is opened, so entries modified after Code.exe started are live.
//! Process start time is obtained via PowerShell `Get-Process`.

use anyhow::Result;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        match self {
            Family::VSCode => "code",
            Family::Cursor => "cursor",
            Family::Windsurf => "windsurf",
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
    let pid = tasklist_pid(spec.process_name)?;
    let storage_root = appdata_dir()?
        .join(spec.appdata_dir)
        .join("User")
        .join("workspaceStorage");

    // Get the earliest start time of this process family so we can filter
    // workspace storage entries to those modified during the current session.
    let process_name_no_ext = spec.process_name.trim_end_matches(".exe");
    let session_start = process_start_time_unix(process_name_no_ext);

    let open_dirs = active_workspace_dirs(&storage_root, session_start);
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

/// Return workspace storage dirs whose `state.vscdb` was modified at or after
/// `session_start_secs` (Unix timestamp). If the process start time could not
/// be determined, falls back to entries modified within the last 24 hours.
fn active_workspace_dirs(storage_root: &PathBuf, session_start_secs: Option<u64>) -> Vec<PathBuf> {
    let cutoff = match session_start_secs {
        Some(t) => UNIX_EPOCH + Duration::from_secs(t),
        None => SystemTime::now() - Duration::from_secs(86400),
    };

    let entries = match fs::read_dir(storage_root) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let db = path.join("state.vscdb");
        let modified = fs::metadata(&db)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        if modified >= cutoff {
            dirs.push(path);
        }
    }
    dirs
}

/// Return the earliest start time of any running process with the given name
/// as a Unix timestamp (seconds), using PowerShell `Get-Process`.
fn process_start_time_unix(name: &str) -> Option<u64> {
    let script = format!(
        "$p = Get-Process -Name '{}' -ErrorAction SilentlyContinue | \
         Sort-Object StartTime | Select-Object -First 1; \
         if ($p) {{ [int64](($p.StartTime.ToUniversalTime() - \
         [datetime]'1970-01-01').TotalSeconds) }}",
        name
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()
}

#[derive(Deserialize)]
struct WorkspaceFile {
    folder: Option<String>,
    workspace: Option<String>,
}

fn read_workspace_project(dir: &PathBuf) -> Option<IdeProject> {
    let path = dir.join("workspace.json");
    let contents = fs::read_to_string(&path).ok()?;
    let parsed: WorkspaceFile = serde_json::from_str(&contents).ok()?;
    let uri = parsed.folder.or(parsed.workspace)?;
    let decoded = decode_vscode_uri(&uri);
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
        active: false,
    })
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
    if let Some(id) = project_id {
        if let Some(path) = lookup_project_path(family, id) {
            let cli = family.cli_name();
            let ok = Command::new(cli)
                .args(["--reuse-window", &path])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
            let _ = Command::new("cmd")
                .args(["/c", "start", "", cli, "--reuse-window", &path])
                .status();
            return Ok(());
        }
    }
    let _ = Command::new("cmd")
        .args(["/c", "start", "", family.cli_name()])
        .status();
    Ok(())
}

fn lookup_project_path(family: Family, project_name: &str) -> Option<String> {
    let storage = appdata_dir()?
        .join(family.appdata_dir())
        .join("User")
        .join("workspaceStorage");
    let entries = fs::read_dir(&storage).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ws_json = path.join("workspace.json");
        let Ok(contents) = fs::read_to_string(&ws_json) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<WorkspaceFile>(&contents) else {
            continue;
        };
        let Some(uri) = parsed.folder.or(parsed.workspace) else {
            continue;
        };
        let decoded = decode_vscode_uri(&uri);
        let name = PathBuf::from(&decoded)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == project_name {
            return Some(decoded);
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

/// Find the PID of the first process matching `exe_name` via `tasklist`.
fn tasklist_pid(exe_name: &str) -> Option<u32> {
    let output = Command::new("tasklist")
        .args([
            "/fi",
            &format!("imagename eq {}", exe_name),
            "/fo",
            "csv",
            "/nh",
        ])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // CSV: "Code.exe","1234","Console","1","50,000 K"
        let mut fields = line.splitn(5, ',');
        let _name = fields.next()?;
        let pid_field = fields.next()?;
        let pid_str = pid_field.trim_matches('"');
        if let Ok(pid) = pid_str.parse::<u32>() {
            return Some(pid);
        }
    }
    None
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
