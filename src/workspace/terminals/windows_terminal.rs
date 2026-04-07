//! Windows Terminal detection and focus (Windows only).
//!
//! Detection uses EnumWindows (C# P/Invoke) to find Windows Terminal top-level windows,
//! then Windows UI Automation to enumerate the individual tabs within each window.
//!
//! Focus uses UI Automation SelectionItemPattern (falling back to InvokePattern) to activate
//! the specific tab, then AttachThreadInput + SetForegroundWindow to raise the window.
//! AttachThreadInput is required on Windows 11 where SetForegroundWindow alone is blocked
//! for background processes.

use anyhow::Result;
use std::process::Command;

use crate::workspace::types::{TerminalEmulator, TerminalTab, TerminalWindow};

pub fn detect() -> Result<Option<TerminalEmulator>> {
    let script = r#"
try { Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes } catch {}
try { Add-Type -TypeDefinition '
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class ZestfulWTEnum {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    public static List<string> FindWindows(uint[] pids) {
        var pidSet = new HashSet<uint>(pids);
        var results = new List<string>();
        EnumWindows((hWnd, lp) => {
            if (!IsWindowVisible(hWnd)) return true;
            uint pid; GetWindowThreadProcessId(hWnd, out pid);
            if (!pidSet.Contains(pid)) return true;
            int len = GetWindowTextLength(hWnd);
            if (len == 0) return true;
            var sb = new StringBuilder(len + 1);
            GetWindowText(hWnd, sb, sb.Capacity);
            results.Add(((long)hWnd).ToString() + "|" + sb.ToString());
            return true;
        }, IntPtr.Zero);
        return results;
    }
}' } catch {}

$wtPids = [uint32[]](Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
if ($wtPids.Count -eq 0) { exit }

$tabCond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::TabItem
)

[ZestfulWTEnum]::FindWindows($wtPids) | ForEach-Object {
    $parts = $_ -split '\|', 2
    $hwnd = [long]$parts[0]
    try {
        $ae = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$hwnd)
        $tabs = $ae.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tabCond)
        if ($tabs.Count -gt 0) {
            for ($i = 0; $i -lt $tabs.Count; $i++) {
                "$hwnd|$($i + 1)|$($tabs[$i].Current.Name)"
            }
        } else {
            "$hwnd|1|"
        }
    } catch {
        "$hwnd|1|"
    }
}
"#;

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut windows: std::collections::BTreeMap<String, Vec<TerminalTab>> =
        std::collections::BTreeMap::new();

    for line in stdout.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: hwnd|tabIndex|tabTitle
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() < 3 {
            continue;
        }

        let hwnd = parts[0].to_string();
        let title = parts[2].trim().to_string();

        windows.entry(hwnd).or_default().push(TerminalTab {
            title,
            uri: None,
            tty: None,
            shell_pid: None,
            shell: None,
            cwd: None,
            columns: None,
            rows: None,
        });
    }

    if windows.is_empty() {
        return Ok(None);
    }

    let terminal_windows: Vec<TerminalWindow> = windows
        .into_iter()
        .map(|(id, tabs)| TerminalWindow { id, tabs })
        .collect();

    Ok(Some(TerminalEmulator {
        app: "Windows Terminal".into(),
        pid: None,
        windows: terminal_windows,
    }))
}

/// Focus a specific Windows Terminal tab by window handle and 1-based tab index.
pub async fn focus(window_id: &str, tab_id: Option<&str>) -> Result<()> {
    let window_id = window_id.to_string();
    let tab_id = tab_id.map(String::from);
    tokio::task::spawn_blocking(move || focus_sync(&window_id, tab_id.as_deref())).await??;
    Ok(())
}

fn focus_sync(window_id: &str, tab_id: Option<&str>) -> Result<()> {
    let hwnd: i64 = match window_id.parse() {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };

    let tab_index: u32 = tab_id.and_then(|t| t.parse().ok()).unwrap_or(1);

    let script = format!(
        r#"
try {{ Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes }} catch {{}}
try {{ Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ZestfulWT {{ [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p); [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool f); }}' }} catch {{}}

$hwnd = [IntPtr]{hwnd}
[ZestfulWT]::ShowWindow($hwnd, 9)
$d = [uint32]0
$fg = [ZestfulWT]::GetWindowThreadProcessId([ZestfulWT]::GetForegroundWindow(), [ref]$d)
$tgt = [ZestfulWT]::GetWindowThreadProcessId($hwnd, [ref]$d)
[ZestfulWT]::AttachThreadInput($fg, $tgt, $true)
[ZestfulWT]::SetForegroundWindow($hwnd)
[ZestfulWT]::BringWindowToTop($hwnd)
[ZestfulWT]::AttachThreadInput($fg, $tgt, $false)

try {{
    $ae = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $tabCond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::TabItem
    )
    $tabs = $ae.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tabCond)
    $idx = {tab_index} - 1
    if ($idx -ge 0 -and $idx -lt $tabs.Count) {{
        $tab = $tabs[$idx]
        try {{
            $pat = $tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
            $pat.Select()
        }} catch {{
            try {{
                $pat = $tab.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
                $pat.Invoke()
            }} catch {{}}
        }}
    }}
}} catch {{}}
"#,
        hwnd = hwnd,
        tab_index = tab_index
    );

    let _ = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_no_panic() {
        let _ = detect();
    }

    #[tokio::test]
    async fn test_focus_no_panic() {
        let result = focus("99999", None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_with_tab_no_panic() {
        let result = focus("99999", Some("1")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_focus_non_numeric_hwnd() {
        let result = focus("not-a-hwnd", None).await;
        assert!(result.is_ok());
    }
}
