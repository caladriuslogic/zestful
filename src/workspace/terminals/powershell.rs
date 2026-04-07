//! PowerShell detection and focus handler (Windows only).

use anyhow::Result;
use std::process::Command;

use crate::workspace::process;
use crate::workspace::types::{TerminalEmulator, TerminalTab, TerminalWindow};

pub fn detect() -> Result<Option<TerminalEmulator>> {
    let windows = collect_windows();
    if windows.is_empty() {
        return Ok(None);
    }

    let first_pid = windows
        .first()
        .and_then(|w| w.tabs.first())
        .and_then(|t| t.shell_pid);

    Ok(Some(TerminalEmulator {
        app: "PowerShell".into(),
        pid: first_pid,
        windows,
    }))
}

fn collect_windows() -> Vec<TerminalWindow> {
    let mut windows = Vec::new();

    for &exe_name in &["powershell.exe", "pwsh.exe"] {
        let entries = query_tasklist(exe_name);
        for (pid, title) in entries {
            let cwd = process::get_cwd(pid);
            let shell = if exe_name == "pwsh.exe" {
                "pwsh"
            } else {
                "powershell"
            };
            let tab = TerminalTab {
                title: if title.is_empty() {
                    shell.to_string()
                } else {
                    title
                },
                uri: None,
                tty: None,
                shell_pid: Some(pid),
                shell: Some(shell.to_string()),
                cwd,
                columns: None,
                rows: None,
            };
            windows.push(TerminalWindow {
                id: pid.to_string(),
                tabs: vec![tab],
            });
        }
    }

    windows
}

/// Query tasklist for a specific executable and return (pid, window_title) pairs.
fn query_tasklist(exe_name: &str) -> Vec<(u32, String)> {
    let output = Command::new("tasklist")
        .args([
            "/fi",
            &format!("imagename eq {}", exe_name),
            "/fo",
            "csv",
            "/v",
            "/nh",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('"') {
            continue;
        }

        let stripped = line
            .strip_prefix('"')
            .unwrap_or(line)
            .trim_end_matches('"');
        let fields: Vec<&str> = stripped.split("\",\"").collect();

        if fields.len() < 9 {
            continue;
        }

        let pid: u32 = match fields[1].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let title = fields[8].trim();
        let title = if title == "N/A" || title == "OleMainThreadWndName" {
            String::new()
        } else {
            title.to_string()
        };

        results.push((pid, title));
    }

    results
}

/// Focus a PowerShell window.
pub async fn focus(window_id: Option<&str>) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        tokio::task::spawn_blocking({
            let window_id = window_id.map(String::from);
            move || focus_sync(window_id.as_deref())
        })
        .await??;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window_id;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn focus_sync(window_id: Option<&str>) -> Result<()> {
    // Three strategies for finding the window on Windows 10 and 11:
    //
    // 1. Direct window or conhost child via EnumWindows — covers Windows 10 and Windows 11
    //    when the user has opted out of Windows Terminal as the default terminal.
    //
    // 2. Walk the parent-process chain looking for WindowsTerminal.exe / OpenConsole.exe —
    //    covers Windows 11 when Windows Terminal spawned powershell.exe directly (e.g. "New Tab").
    //
    // 3. Any visible WindowsTerminal.exe window — covers Windows 11 "defterm" interception,
    //    where WT intercepts a standalone powershell.exe launch with no direct parent relationship.
    //
    // AttachThreadInput is also required on Windows 11 to bypass the tighter foreground lock.
    let add_type = r#"try { Add-Type -TypeDefinition '
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public class ZestfulWin32 {
    delegate bool EWP(IntPtr h, IntPtr l);
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Ansi)]
    struct PE32 { public uint sz, use, pid; public IntPtr heap; public uint mod, thr, ppid; public int pri; public uint flags; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=260)] public string exe; }
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int n);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint a, uint b, bool f);
    [DllImport("user32.dll")] static extern bool EnumWindows(EWP cb, IntPtr l);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("kernel32.dll", CharSet=CharSet.Ansi)] static extern IntPtr CreateToolhelp32Snapshot(uint f, uint p);
    [DllImport("kernel32.dll", CharSet=CharSet.Ansi)] static extern bool Process32First(IntPtr s, ref PE32 e);
    [DllImport("kernel32.dll", CharSet=CharSet.Ansi)] static extern bool Process32Next(IntPtr s, ref PE32 e);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    static IntPtr FindVisibleWindow(HashSet<uint> pids) {
        IntPtr r = IntPtr.Zero;
        EWP cb = (h, l) => { if (!IsWindowVisible(h)) return true; uint p2; GetWindowThreadProcessId(h, out p2); if (pids.Contains(p2)) { r = h; return false; } return true; };
        EnumWindows(cb, IntPtr.Zero);
        return r;
    }
    static void RaiseWindow(IntPtr hwnd) {
        ShowWindow(hwnd, 9);
        uint d; uint fg = GetWindowThreadProcessId(GetForegroundWindow(), out d); uint tgt = GetWindowThreadProcessId(hwnd, out d);
        AttachThreadInput(fg, tgt, true); SetForegroundWindow(hwnd); BringWindowToTop(hwnd); AttachThreadInput(fg, tgt, false);
    }
    public static void Focus(uint pid) {
        var childPids = new HashSet<uint> { pid };
        var procMap = new Dictionary<uint, KeyValuePair<uint, string>>();
        IntPtr snap = CreateToolhelp32Snapshot(2, 0);
        if (snap != new IntPtr(-1)) {
            var e = new PE32(); e.sz = (uint)Marshal.SizeOf(e);
            if (Process32First(snap, ref e)) do {
                string x = e.exe != null ? e.exe.ToLower() : "";
                procMap[e.pid] = new KeyValuePair<uint, string>(e.ppid, x);
                if (e.ppid == pid && x == "conhost.exe") childPids.Add(e.pid);
            } while (Process32Next(snap, ref e));
            CloseHandle(snap);
        }
        IntPtr hwnd = FindVisibleWindow(childPids);
        if (hwnd == IntPtr.Zero) {
            uint cur = pid;
            for (int i = 0; i < 4 && hwnd == IntPtr.Zero; i++) {
                KeyValuePair<uint, string> entry;
                if (!procMap.TryGetValue(cur, out entry) || entry.Key == 0) break;
                cur = entry.Key;
                KeyValuePair<uint, string> pe;
                if (!procMap.TryGetValue(cur, out pe)) break;
                if (pe.Value == "windowsterminal.exe" || pe.Value == "openconsole.exe") hwnd = FindVisibleWindow(new HashSet<uint> { cur });
            }
        }
        if (hwnd == IntPtr.Zero) {
            var wtPids = new HashSet<uint>();
            foreach (var kv in procMap) if (kv.Value.Value == "windowsterminal.exe") wtPids.Add(kv.Key);
            if (wtPids.Count > 0) hwnd = FindVisibleWindow(wtPids);
        }
        if (hwnd != IntPtr.Zero) RaiseWindow(hwnd);
    }
}' } catch {}"#;

    let find_proc = match window_id {
        Some(pid) if pid.chars().all(|c| c.is_ascii_digit()) => format!(
            "$p = Get-Process -Id {} -ErrorAction SilentlyContinue",
            pid
        ),
        _ => String::from(
            "$p = Get-Process -Name powershell,pwsh -ErrorAction SilentlyContinue | Select-Object -First 1"
        ),
    };

    let script = format!(
        "{add_type}; {find_proc}; if ($p) {{ [ZestfulWin32]::Focus([uint32]$p.Id) }}"
    );

    let _ = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_focus_no_panic() {
        let result = focus(None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_pid() {
        let result = focus(Some("1234")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_non_numeric_id() {
        let result = focus(Some("some-id")).await;
        assert!(result.is_ok());
    }
}
