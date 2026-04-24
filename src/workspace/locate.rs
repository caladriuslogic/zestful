use anyhow::Result;
use std::process::Command;

#[cfg(not(target_os = "windows"))]
use crate::workspace::terminals;
#[cfg(not(target_os = "windows"))]
use crate::workspace::types::TerminalEmulator;

/// Determine where the current process is running and return a canonical URI.
pub fn locate() -> Result<String> {
    let mut segments: Vec<String> = Vec::new();

    // VS Code-family integrated terminals: the Zestful VS Code extension drops
    // a state file per window; if our shell PID matches one, we can emit a
    // precise URI like `workspace://vscode/terminal:1234-5`. Falls back to
    // `workspace://<editor>/project:<name>` when the process is inside a
    // VS Code-family editor but not in a tracked integrated terminal (e.g.
    // Cursor's sidebar AI agent).
    #[cfg(target_os = "macos")]
    if segments.is_empty() {
        if let Some(vscode_segments) = detect_vscode_family_terminal() {
            segments.extend(vscode_segments);
        }
    }

    // Kitty sets KITTY_WINDOW_ID in each shell — use it directly
    if let Ok(kitty_win_id) = std::env::var("KITTY_WINDOW_ID") {
        if !kitty_win_id.is_empty() {
            segments.push("kitty".into());
            segments.push(format!("window:{}", kitty_win_id));
        }
    }

    // On Windows, detect the host terminal. Try Windows Terminal first (covers Win 11
    // defterm and Win 10 with WT installed), then fall back to classic cmd/powershell
    // consoles for Windows 10 without WT.
    #[cfg(target_os = "windows")]
    if segments.is_empty() {
        if let Some((hwnd, tab_idx)) = find_windows_terminal() {
            segments.push("windows-terminal".into());
            segments.push(format!("window:{}", hwnd));
            if let Some(idx) = tab_idx {
                segments.push(format!("tab:{}", idx));
            }
        } else if let Some((app, pid)) = find_classic_console() {
            segments.push(app);
            segments.push(format!("window:{}", pid));
        }
    }

    // For non-kitty terminals on Unix, find our TTY and match against detected terminals
    #[cfg(not(target_os = "windows"))]
    if segments.is_empty() {
        let tty = find_our_tty();
        if let Some(tty_name) = &tty {
            if let Some((app, win_id, tab_idx)) = find_terminal_for_tty(tty_name)? {
                segments.push(app.to_lowercase().replace(' ', "-"));
                segments.push(format!("window:{}", win_id));
                if let Some(idx) = tab_idx {
                    segments.push(format!("tab:{}", idx));
                }
            }
        }
    }

    // Detect SSH layer
    if let Some(ssh_segments) = detect_ssh() {
        segments.extend(ssh_segments);
    }

    // Detect multiplexer layers
    if let Some(mux_segments) = detect_tmux()? {
        segments.extend(mux_segments);
    } else if let Some(mux_segments) = detect_zellij()? {
        segments.extend(mux_segments);
    } else if let Some(mux_segments) = detect_shelldon()? {
        segments.extend(mux_segments);
    }

    // If we didn't identify any terminal / IDE / browser, don't emit a
    // placeholder URI — the notification should go through with no
    // terminal_uri (so no Focus button is offered). `workspace://unknown`
    // and `workspace://tty:<name>` are never actionable.
    if segments.is_empty() {
        anyhow::bail!("locate: no recognizable workspace context");
    }

    Ok(format!("workspace://{}", segments.join("/")))
}

/// On Windows, detect which Windows Terminal window/tab the current process is running in.
///
/// Strategy: build the set of all ancestor PIDs for our process, then enumerate every
/// `PseudoConsoleWindow` owned by each WT frame (`CASCADIA_HOSTING_WINDOW_CLASS`).
/// Each `PseudoConsoleWindow`'s owner process is the interactive shell for that tab.
/// If that shell PID appears in our ancestor set we are running inside that tab.
///
/// This handles both the ordinary case (shell is a direct WT child) and the Windows 11
/// "default terminal" (defterm) case where the shell is parented to `explorer.exe` or
/// another launcher — in both cases WT creates a `PseudoConsoleWindow` whose owner is
/// always the actual shell, so the ancestor walk finds it correctly.
/// Only returns `Some` when we are genuinely inside a WT tab.
#[cfg(target_os = "windows")]
fn find_windows_terminal() -> Option<(String, Option<u32>)> {
    let our_pid = std::process::id();
    let script = format!(
        r#"
$wtPids = [uint32[]](Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty Id)
if ($wtPids.Count -eq 0) {{ exit }}

try {{ Add-Type -TypeDefinition '
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class ZestfulLocateWT3 {{
    public delegate bool EWP(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EWP cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);

    // Find the visible CASCADIA_HOSTING_WINDOW_CLASS frame for a WT pid.
    public static long FindFrame(uint wtPid) {{
        long result = 0;
        EWP cb = (h, l) => {{
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (p != wtPid) return true;
            var sb = new StringBuilder(64);
            GetClassName(h, sb, sb.Capacity);
            if (sb.ToString() != "CASCADIA_HOSTING_WINDOW_CLASS") return true;
            result = (long)h; return false;
        }};
        EnumWindows(cb, IntPtr.Zero);
        return result;
    }}

    // Return shell PIDs for all PseudoConsoleWindows parented to frameHwnd.
    public static List<uint> FindShellPids(long frameHwnd) {{
        var results = new List<uint>();
        EnumWindows((h, l) => {{
            if ((long)GetParent(h) != frameHwnd) return true;
            var sb = new StringBuilder(64);
            GetClassName(h, sb, sb.Capacity);
            if (sb.ToString() != "PseudoConsoleWindow") return true;
            uint pid; GetWindowThreadProcessId(h, out pid);
            results.Add(pid);
            return true;
        }}, IntPtr.Zero);
        return results;
    }}
}}' }} catch {{ exit }}

# Build a flat process map: pid -> parentPid
$procMap = @{{}}
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | ForEach-Object {{
    $procMap[[uint32]$_.ProcessId] = [uint32]$_.ParentProcessId
}}

# Collect all ancestor PIDs of our process (including our own PID).
$ancestors = [System.Collections.Generic.HashSet[uint32]]::new()
$cur = [uint32]{our_pid}
for ($i = 0; $i -lt 20 -and $cur -gt 1; $i++) {{
    [void]$ancestors.Add($cur)
    $ppid = $procMap[$cur]
    if (-not $ppid -or $ppid -eq $cur) {{ break }}
    $cur = $ppid
}}

# For each WT process, check if any PseudoConsoleWindow shell is our ancestor.
foreach ($wtPid in $wtPids) {{
    $frameHwnd = [ZestfulLocateWT3]::FindFrame($wtPid)
    if ($frameHwnd -eq 0) {{ continue }}
    $shellPids = [ZestfulLocateWT3]::FindShellPids($frameHwnd)
    foreach ($spid in $shellPids) {{
        if ($ancestors.Contains($spid)) {{
            [System.Console]::Error.WriteLine("pcw: frame=$frameHwnd shell_pid=$spid")
            Write-Output "$frameHwnd|$spid"
            exit
        }}
    }}
}}
"#,
        our_pid = our_pid
    );

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.trim().lines() {
        let line = line.trim();
        if !line.is_empty() {
            crate::log::log("wt-locate", line);
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    if line.is_empty() {
        return None;
    }

    let parts: Vec<&str> = line.splitn(2, '|').collect();
    if parts.len() < 2 {
        return None;
    }

    let hwnd = parts[0].trim().to_string();
    let shell_pid: u32 = parts[1].trim().parse().unwrap_or(0);
    let tab_id = if shell_pid != 0 {
        Some(shell_pid)
    } else {
        None
    };

    crate::log::log(
        "wt-locate",
        &format!("result: hwnd={} shell_pid={:?}", hwnd, tab_id),
    );
    Some((hwnd, tab_id))
}

/// On Windows 10 without Windows Terminal, walk the parent process chain to find a
/// classic cmd.exe or powershell.exe console host. The PID is used as the window ID,
/// matching the format produced by cmd::detect() and powershell::detect().
/// Returns (app_slug, pid_string), e.g. ("cmd", "1234") or ("powershell", "5678").
#[cfg(target_os = "windows")]
fn find_classic_console() -> Option<(String, String)> {
    // Pass our own PID explicitly — do NOT use $PID inside the script, which is the
    // spawned powershell.exe process and would match itself as "powershell" immediately.
    let our_pid = std::process::id();
    let script = format!(
        r#"
$procMap = @{{}}
Get-CimInstance Win32_Process | ForEach-Object {{
    $procMap[[uint32]$_.ProcessId] = [PSCustomObject]@{{ ppid = [uint32]$_.ParentProcessId; name = $_.Name.ToLower() }}
}}
$cur = [uint32]{our_pid}
for ($i = 0; $i -lt 10 -and $cur -gt 1; $i++) {{
    $entry = $procMap[$cur]
    if (-not $entry) {{ break }}
    if ($entry.name -eq 'cmd.exe') {{ Write-Output "cmd|$cur"; exit }}
    if ($entry.name -eq 'powershell.exe' -or $entry.name -eq 'pwsh.exe') {{ Write-Output "powershell|$cur"; exit }}
    $cur = $entry.ppid
}}
"#
    );

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    if line.is_empty() {
        return None;
    }

    let parts: Vec<&str> = line.splitn(2, '|').collect();
    if parts.len() < 2 {
        return None;
    }

    Some((parts[0].trim().to_string(), parts[1].trim().to_string()))
}

/// Walk up the process tree from our PID to find a TTY.
fn find_our_tty() -> Option<String> {
    let pid = std::process::id();
    let mut current_pid = pid;

    for _ in 0..20 {
        let output = Command::new("ps")
            .args(["-p", &current_pid.to_string(), "-o", "tty=,ppid="])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            return None;
        }

        let tty = parts[0];
        let ppid: u32 = parts[1].parse().ok()?;

        if tty != "??" && tty != "?" && !tty.is_empty() {
            return Some(format!("/dev/{}", tty));
        }

        if ppid == 0 || ppid == 1 || ppid == current_pid {
            return None;
        }
        current_pid = ppid;
    }
    None
}

/// Match a TTY against detected terminal emulators to find which app/window/tab owns it.
#[cfg(not(target_os = "windows"))]
fn find_terminal_for_tty(tty: &str) -> Result<Option<(String, String, Option<u32>)>> {
    let terminals = terminals::detect_all()?;

    // For tmux, we need the TTY of the tmux client, not the pane TTY.
    let tty_to_match = if std::env::var("TMUX").is_ok() {
        find_tmux_client_tty().unwrap_or_else(|| tty.to_string())
    } else if let Ok(client_tty) = std::env::var("SHELLDON_CLIENT_TTY") {
        if !client_tty.is_empty() {
            client_tty
        } else {
            tty.to_string()
        }
    } else {
        tty.to_string()
    };

    for term in &terminals {
        for win in &term.windows {
            for (i, tab) in win.tabs.iter().enumerate() {
                if let Some(tab_tty) = &tab.tty {
                    if *tab_tty == tty_to_match {
                        return Ok(Some((
                            term.app.clone(),
                            win.id.clone(),
                            Some((i + 1) as u32),
                        )));
                    }
                }
            }
        }
    }

    // If we didn't match and we're in shelldon, try matching shelldon's TTY
    if std::env::var("SHELLDON_RUNTIME").is_ok() {
        let shelldon_tty = find_shelldon_tty();
        if let Some(stty) = &shelldon_tty {
            if stty != &tty_to_match {
                return find_terminal_for_tty_inner(&terminals, stty);
            }
        }
    }

    Ok(None)
}

#[cfg(not(target_os = "windows"))]
fn find_terminal_for_tty_inner(
    terminals: &[TerminalEmulator],
    tty: &str,
) -> Result<Option<(String, String, Option<u32>)>> {
    for term in terminals {
        for win in &term.windows {
            for (i, tab) in win.tabs.iter().enumerate() {
                if let Some(tab_tty) = &tab.tty {
                    if *tab_tty == *tty {
                        return Ok(Some((
                            term.app.clone(),
                            win.id.clone(),
                            Some((i + 1) as u32),
                        )));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Find the TTY of the tmux client (the terminal tab that tmux is running in).
#[cfg(not(target_os = "windows"))]
fn find_tmux_client_tty() -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{client_tty}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let tty = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tty.is_empty() {
        None
    } else {
        Some(tty)
    }
}

/// Find the TTY of the parent shelldon process.
#[cfg(not(target_os = "windows"))]
fn find_shelldon_tty() -> Option<String> {
    let pid = std::process::id();
    let mut current_pid = pid;

    for _ in 0..20 {
        let output = Command::new("ps")
            .args(["-p", &current_pid.to_string(), "-o", "ppid=,comm=,tty="])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
        if parts.len() < 3 {
            return None;
        }

        let ppid: u32 = parts[0].trim().parse().ok()?;
        let comm = parts[1].trim();
        let tty = parts[2].trim();

        if (comm.contains("shelldon") || comm == "-shelldon") && tty != "??" && !tty.is_empty() {
            return Some(format!("/dev/{}", tty));
        }

        if ppid == 0 || ppid == 1 || ppid == current_pid {
            return None;
        }
        current_pid = ppid;
    }
    None
}

/// Detect if we're inside an SSH session.
/// Match result from the VS Code-family detector.
#[cfg(target_os = "macos")]
enum VSCodeMatch {
    /// Exact integrated-terminal hit: `workspace://<slug>/terminal:<id>`
    Terminal { slug: String, terminal_id: String },
    /// No terminal match but we're inside the editor's process tree, so we
    /// at least know the editor, its open workspace folder, and the
    /// extension-host window PID.
    Project { slug: String, project_name: String, window_pid: u32 },
}

#[cfg(target_os = "macos")]
fn detect_vscode_family_terminal() -> Option<Vec<String>> {
    match detect_vscode_family()? {
        VSCodeMatch::Terminal { slug, terminal_id } => {
            Some(vec![slug, format!("terminal:{}", terminal_id)])
        }
        VSCodeMatch::Project { slug, project_name, window_pid } => {
            Some(vec![
                slug,
                format!("window:{}", window_pid),
                format!("project:{}", project_name),
            ])
        }
    }
}

/// Walk the process tree from our PID; if any ancestor matches a `shellPid`
/// in any VS Code-family extension state file at
/// `~/.config/zestful/vscode/*.json`, return a Terminal match. Otherwise,
/// if an ancestor matches a recorded extension-host `windowPid`, return a
/// Project match so non-terminal agents (e.g. Cursor's sidebar AI) still
/// emit a useful URI.
#[cfg(target_os = "macos")]
fn detect_vscode_family() -> Option<VSCodeMatch> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct StateFile {
        #[serde(rename = "windowPid")]
        window_pid: Option<u32>,
        #[serde(rename = "appName")]
        app_name: Option<String>,
        #[serde(rename = "workspaceFolder")]
        workspace_folder: Option<String>,
        terminals: Option<Vec<TerminalEntry>>,
    }
    #[derive(Deserialize)]
    struct TerminalEntry {
        id: String,
        #[serde(rename = "shellPid")]
        shell_pid: Option<u32>,
    }

    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)?
        .join(".config/zestful/vscode");
    let entries = std::fs::read_dir(&dir).ok()?;

    let mut term_by_pid: std::collections::HashMap<u32, (String, String)> =
        std::collections::HashMap::new();
    // windowPid → (slug, project_name) for the project-level fallback.
    let mut window_by_pid: std::collections::HashMap<u32, (String, String)> =
        std::collections::HashMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(state) = serde_json::from_str::<StateFile>(&contents) else {
            continue;
        };
        let app = state.app_name.unwrap_or_default();
        let slug = match app.as_str() {
            "Cursor" => "cursor".to_string(),
            "Windsurf" => "windsurf".to_string(),
            _ => "vscode".to_string(),
        };
        if let Some(wpid) = state.window_pid {
            let project_name = state
                .workspace_folder
                .as_deref()
                .and_then(|p| {
                    std::path::Path::new(p)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            if !project_name.is_empty() {
                window_by_pid.insert(wpid, (slug.clone(), project_name));
            }
        }
        for term in state.terminals.unwrap_or_default() {
            if let Some(pid) = term.shell_pid {
                term_by_pid.insert(pid, (slug.clone(), term.id));
            }
        }
    }

    if term_by_pid.is_empty() && window_by_pid.is_empty() {
        crate::log::log("locate", "vscode-family: no VS Code-family state files");
        return None;
    }

    // Walk up, checking terminal match first (more specific), then window match.
    let start_pid = std::process::id();
    let mut current = start_pid;
    let mut chain = Vec::new();
    for _ in 0..30 {
        chain.push(current);
        if let Some((slug, id)) = term_by_pid.get(&current) {
            crate::log::log(
                "locate",
                &format!(
                    "vscode-family: matched terminal shellPid={} → {}/{}",
                    current, slug, id
                ),
            );
            return Some(VSCodeMatch::Terminal {
                slug: slug.clone(),
                terminal_id: id.clone(),
            });
        }
        if let Some((slug, name)) = window_by_pid.get(&current) {
            crate::log::log(
                "locate",
                &format!(
                    "vscode-family: matched extension-host windowPid={} → {}/project:{}",
                    current, slug, name
                ),
            );
            return Some(VSCodeMatch::Project {
                slug: slug.clone(),
                project_name: name.clone(),
                window_pid: current,
            });
        }
        let output = std::process::Command::new("ps")
            .args(["-p", &current.to_string(), "-o", "ppid="])
            .output()
            .ok()?;
        let ppid: u32 = std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse()
            .ok()?;
        if ppid == 0 || ppid == 1 || ppid == current {
            break;
        }
        current = ppid;
    }
    crate::log::log(
        "locate",
        &format!(
            "vscode-family: no match. start={} chain={:?} terms={:?} windows={:?}",
            start_pid,
            chain,
            term_by_pid.keys().collect::<Vec<_>>(),
            window_by_pid.keys().collect::<Vec<_>>()
        ),
    );
    None
}

/// Scan the Zestful VS Code extension's state files for any window that is
/// reporting an active Codex conversation tab. Returns the editor slug,
/// workspace-folder basename, and extension-host window PID of the best
/// match (preferring a window where the Codex tab is the active tab over
/// one where it is merely open). State files without a recorded windowPid
/// are skipped — we can't route without it.
///
/// Used by `zestful hook` to correlate a Codex hook — which always fires
/// from the shared `Codex.app` daemon regardless of UI surface — back to
/// the VS Code-family window the user is actually driving it from.
#[cfg(target_os = "macos")]
pub fn find_active_codex_editor() -> Option<(String, String, u32)> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct StateFile {
        #[serde(rename = "windowPid")]
        window_pid: Option<u32>,
        #[serde(rename = "appName")]
        app_name: Option<String>,
        #[serde(rename = "workspaceFolder")]
        workspace_folder: Option<String>,
        codex: Option<CodexState>,
    }
    #[derive(Deserialize)]
    struct CodexState {
        #[serde(rename = "tabOpen")]
        tab_open: Option<bool>,
        #[serde(rename = "tabActive")]
        tab_active: Option<bool>,
    }

    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)?
        .join(".config/zestful/vscode");
    let entries = std::fs::read_dir(&dir).ok()?;

    // (tab_active, slug, project, window_pid)
    let mut best: Option<(bool, String, String, u32)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(state) = serde_json::from_str::<StateFile>(&contents) else {
            continue;
        };
        let Some(codex) = state.codex else { continue };
        let tab_active = codex.tab_active.unwrap_or(false);
        let tab_open = codex.tab_open.unwrap_or(false);
        if !tab_open && !tab_active {
            continue;
        }
        // Skip state files that don't record a windowPid — we can't
        // route without it.
        let Some(window_pid) = state.window_pid else {
            continue;
        };
        let slug = match state.app_name.as_deref().unwrap_or("") {
            "Cursor" => "cursor",
            "Windsurf" => "windsurf",
            _ => "vscode",
        };
        let project = state
            .workspace_folder
            .as_deref()
            .and_then(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        if project.is_empty() {
            continue;
        }
        let candidate = (tab_active, slug.to_string(), project, window_pid);
        best = match best {
            None => Some(candidate),
            Some(prev) if !prev.0 && candidate.0 => Some(candidate), // prefer active
            other => other,
        };
    }
    best.map(|(_, slug, project, window_pid)| (slug, project, window_pid))
}

#[cfg(not(target_os = "macos"))]
pub fn find_active_codex_editor() -> Option<(String, String, u32)> {
    None
}

fn detect_ssh() -> Option<Vec<String>> {
    let ssh_conn = std::env::var("SSH_CONNECTION").ok()?;

    let parts: Vec<&str> = ssh_conn.split_whitespace().collect();
    let client_ip = parts.first().copied().unwrap_or("unknown");

    let hostname = Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into());

    Some(vec![format!(
        "ssh:{}@{}(from:{})",
        user, hostname, client_ip
    )])
}

fn detect_tmux() -> Result<Option<Vec<String>>> {
    let tmux_env = std::env::var("TMUX");
    if tmux_env.is_err() {
        return Ok(None);
    }

    // TMUX_PANE is set by tmux for each pane and inherited by child processes,
    // including hooks spawned by agents. Use it to query the specific pane we're
    // running in, rather than the currently focused pane.
    if let Ok(pane_id) = std::env::var("TMUX_PANE") {
        if !pane_id.is_empty() {
            let output = Command::new("tmux")
                .args([
                    "display-message",
                    "-t",
                    &pane_id,
                    "-p",
                    "#{session_name}\t#{window_index}\t#{pane_index}",
                ])
                .output();

            if let Ok(ref o) = output {
                if o.status.success() {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let parts: Vec<&str> = stdout.trim().split('\t').collect();
                    if parts.len() >= 3 {
                        return Ok(Some(vec![
                            format!("tmux:{}", parts[0]),
                            format!("window:{}", parts[1]),
                            format!("pane:{}", parts[2]),
                        ]));
                    }
                }
            }
        }
    }

    // Fallback: use display-message without target (returns the focused pane)
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#{session_name}\t#{window_index}\t#{pane_index}",
        ])
        .output()?;

    if !output.status.success() {
        return Ok(Some(vec!["tmux".into()]));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('\t').collect();
    if parts.len() >= 3 {
        Ok(Some(vec![
            format!("tmux:{}", parts[0]),
            format!("window:{}", parts[1]),
            format!("pane:{}", parts[2]),
        ]))
    } else {
        Ok(Some(vec!["tmux".into()]))
    }
}

fn detect_zellij() -> Result<Option<Vec<String>>> {
    let session = std::env::var("ZELLIJ_SESSION_NAME");
    if session.is_err() {
        return Ok(None);
    }

    let session = session.unwrap();
    let mut segments = vec![format!("zellij:{}", session)];

    let output = Command::new("zellij")
        .args(["action", "list-panes", "--json", "--all"])
        .output();

    if let Ok(o) = output {
        if o.status.success() {
            let raw: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap_or_default();
            for p in &raw {
                let focused = p
                    .get("FOCUSED")
                    .or_else(|| p.get("focused"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if focused {
                    if let Some(tab) = p
                        .get("TAB_POS")
                        .or_else(|| p.get("tab_pos"))
                        .and_then(|v| v.as_u64())
                    {
                        segments.push(format!("tab:{}", tab));
                    }
                    if let Some(pane) = p
                        .get("PANE_ID")
                        .or_else(|| p.get("pane_id"))
                        .and_then(|v| v.as_u64())
                    {
                        segments.push(format!("pane:{}", pane));
                    }
                    break;
                }
            }
        }
    }

    Ok(Some(segments))
}

fn detect_shelldon() -> Result<Option<Vec<String>>> {
    if std::env::var("SHELLDON_RUNTIME").is_err() {
        return Ok(None);
    }

    let pid = std::process::id();
    let mut current_pid = pid;

    for _ in 0..20 {
        let output = Command::new("ps")
            .args(["-p", &current_pid.to_string(), "-o", "ppid=,comm="])
            .output()?;

        if !output.status.success() {
            break;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
        if parts.len() < 2 {
            break;
        }

        let ppid: u32 = parts[0].trim().parse().unwrap_or(0);
        let comm = parts[1].trim();

        if comm.contains("shelldon") || comm == "-shelldon" {
            for check_pid in [current_pid, ppid] {
                let discovery_path = format!("/tmp/shelldon-{}.json", check_pid);
                if let Ok(contents) = std::fs::read_to_string(&discovery_path) {
                    if let Ok(info) = serde_json::from_str::<serde_json::Value>(&contents) {
                        let session_id = info
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let mut segments = vec![format!("shelldon:{}", session_id)];

                        if let Ok(pane_id) = std::env::var("SHELLDON_PANE_ID") {
                            segments.push(format!("pane:{}", pane_id));
                        }
                        if let Ok(tab_id) = std::env::var("SHELLDON_TAB_ID") {
                            segments.push(format!("tab:{}", tab_id));
                        }

                        return Ok(Some(segments));
                    }
                }
            }
            return Ok(Some(vec![format!("shelldon:pid-{}", current_pid)]));
        }

        if ppid == 0 || ppid == 1 || ppid == current_pid {
            break;
        }
        current_pid = ppid;
    }

    Ok(Some(vec!["shelldon".into()]))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use std::io::Write;

    /// Temporarily redirect $HOME to a tempdir, restore on drop.
    /// Mirrors the HomeGuard pattern in src/cmd/daemon.rs tests.
    struct HomeGuard {
        old: Option<String>,
        _td: tempfile::TempDir,
    }
    impl HomeGuard {
        fn new() -> Self {
            let td = tempfile::TempDir::new().unwrap();
            let old = std::env::var("HOME").ok();
            // SAFETY: tests run with --test-threads=1.
            unsafe { std::env::set_var("HOME", td.path()); }
            HomeGuard { old, _td: td }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: single-threaded tests.
            unsafe {
                match &self.old {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn seed_state_file(filename: &str, body: &str) {
        let home = std::env::var_os("HOME").unwrap();
        let dir = std::path::PathBuf::from(home).join(".config/zestful/vscode");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(filename);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    #[serial]
    fn find_active_codex_editor_returns_window_pid() {
        let _home = HomeGuard::new();
        seed_state_file(
            "80836.json",
            r#"{
                "windowPid": 80836,
                "appName": "Visual Studio Code",
                "workspaceFolder": "/Users/x/Development/zestful",
                "codex": { "installed": true, "tabOpen": true, "tabActive": true }
            }"#,
        );

        let result = find_active_codex_editor();
        assert_eq!(
            result,
            Some(("vscode".to_string(), "zestful".to_string(), 80836))
        );
    }

    #[test]
    #[serial]
    fn find_active_codex_editor_prefers_active_tab_over_open() {
        let _home = HomeGuard::new();
        seed_state_file(
            "100.json",
            r#"{
                "windowPid": 100,
                "appName": "Visual Studio Code",
                "workspaceFolder": "/x/Project-Open",
                "codex": { "installed": true, "tabOpen": true, "tabActive": false }
            }"#,
        );
        seed_state_file(
            "200.json",
            r#"{
                "windowPid": 200,
                "appName": "Visual Studio Code",
                "workspaceFolder": "/x/Project-Active",
                "codex": { "installed": true, "tabOpen": true, "tabActive": true }
            }"#,
        );

        let result = find_active_codex_editor();
        assert_eq!(
            result,
            Some(("vscode".to_string(), "Project-Active".to_string(), 200))
        );
    }

    #[test]
    #[serial]
    fn find_active_codex_editor_skips_state_without_window_pid() {
        let _home = HomeGuard::new();
        seed_state_file(
            "orphan.json",
            r#"{
                "appName": "Visual Studio Code",
                "workspaceFolder": "/x/NoPid",
                "codex": { "installed": true, "tabOpen": true, "tabActive": true }
            }"#,
        );

        assert_eq!(find_active_codex_editor(), None);
    }

    #[test]
    #[serial]
    fn find_active_codex_editor_maps_cursor_and_windsurf_app_names() {
        let _home = HomeGuard::new();
        seed_state_file(
            "300.json",
            r#"{
                "windowPid": 300,
                "appName": "Cursor",
                "workspaceFolder": "/x/CursorProj",
                "codex": { "installed": true, "tabOpen": true, "tabActive": true }
            }"#,
        );

        let result = find_active_codex_editor();
        assert_eq!(
            result,
            Some(("cursor".to_string(), "CursorProj".to_string(), 300))
        );
    }
}
