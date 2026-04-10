use anyhow::Result;
use std::process::Command;

#[cfg(not(target_os = "windows"))]
use crate::workspace::terminals;
#[cfg(not(target_os = "windows"))]
use crate::workspace::types::TerminalEmulator;

/// Determine where the current process is running and return a canonical URI.
pub fn locate() -> Result<String> {
    let mut segments: Vec<String> = Vec::new();

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

    if segments.is_empty() {
        if let Some(tty_name) = find_our_tty() {
            segments.push(format!("tty:{}", tty_name.replace("/dev/", "")));
        } else {
            segments.push("unknown".into());
        }
    }

    Ok(format!("workspace://{}", segments.join("/")))
}

/// On Windows, detect the active Windows Terminal window and its selected tab.
///
/// On Windows 11 with the default terminal ("defterm") setting, WT intercepts console
/// creation at the OS level — the shell process's parent is `explorer.exe`, not
/// `WindowsTerminal.exe`. Walking the parent chain therefore never finds WT. Instead,
/// we check directly whether `WindowsTerminal.exe` is running and grab its visible
/// window + selected tab via UI Automation (strategy 3 from the focus code).
///
/// Known limitation: if multiple WT windows are open this returns whichever EnumWindows
/// enumerates first. Accurate per-window matching would require ConPTY-level introspection.
#[cfg(target_os = "windows")]
fn find_windows_terminal() -> Option<(String, Option<u32>)> {
    let script = r#"
$wtPids = [uint32[]](Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty Id)
if ($wtPids.Count -eq 0) { exit }

try { Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes } catch {}
try { Add-Type -TypeDefinition '
using System; using System.Runtime.InteropServices;
public class ZestfulLocateWT2 {
    public delegate bool EWP(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EWP cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    public static long FindHwnd(uint[] pids) {
        var pidSet = new System.Collections.Generic.HashSet<uint>(pids);
        long result = 0;
        EWP cb = (h, l) => {
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (pidSet.Contains(p) && GetWindowTextLength(h) > 0) { result = (long)h; return false; }
            return true;
        };
        EnumWindows(cb, IntPtr.Zero);
        return result;
    }
}' } catch {}

$hwnd = [ZestfulLocateWT2]::FindHwnd($wtPids)
if ($hwnd -eq 0) { exit }

try {
    $ae = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$hwnd)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::TabItem)
    $tabs = $ae.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
    $tabIdx = 1
    for ($i = 0; $i -lt $tabs.Count; $i++) {
        try {
            $sel = $tabs[$i].GetCurrentPattern(
                [System.Windows.Automation.SelectionItemPattern]::Pattern)
            if ($sel.Current.IsSelected) { $tabIdx = $i + 1; break }
        } catch {}
    }
    Write-Output "$hwnd|$tabIdx"
} catch {
    Write-Output "$hwnd|1"
}
"#;

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
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

    let hwnd = parts[0].trim().to_string();
    let tab_idx: u32 = parts[1].trim().parse().ok()?;

    Some((hwnd, Some(tab_idx)))
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
    let script = format!(r#"
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
"#);

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
        if !client_tty.is_empty() { client_tty } else { tty.to_string() }
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

        if (comm.contains("shelldon") || comm == "-shelldon")
            && tty != "??"
            && !tty.is_empty()
        {
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
            let raw: Vec<serde_json::Value> =
                serde_json::from_slice(&o.stdout).unwrap_or_default();
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
