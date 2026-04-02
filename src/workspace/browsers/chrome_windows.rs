//! Google Chrome detection and focus for Windows.
//!
//! Detection uses EnumWindows (C# P/Invoke) to find Chrome top-level windows,
//! then Windows UI Automation to enumerate the individual tabs within each
//! window — no remote debugging port required.
//!
//! Focus uses UI Automation SelectionItemPattern (falling back to InvokePattern)
//! to activate the specific tab, then Win32 ShowWindow+SetForegroundWindow to
//! restore and raise the window.

use anyhow::Result;
use std::process::Command;

use crate::workspace::types::{BrowserInstance, BrowserTab, BrowserWindow};

pub fn detect() -> Result<Option<BrowserInstance>> {
    let script = r#"
try { Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes } catch {}
try { Add-Type -TypeDefinition '
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class ZestfulEnum {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    public static List<string> FindChromeWindows(uint[] pids) {
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
            string title = sb.ToString();
            if (title.EndsWith(" - Google Chrome"))
                results.Add(pid.ToString() + "|" + ((long)hWnd).ToString() + "|" + title);
            return true;
        }, IntPtr.Zero);
        return results;
    }
}' } catch {}

$chromePids = [uint32[]](Get-Process -Name chrome -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
if ($chromePids.Count -eq 0) { exit }

$tabCond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::TabItem
)

[ZestfulEnum]::FindChromeWindows($chromePids) | ForEach-Object {
    $parts = $_ -split '\|', 3
    $hwnd = [long]$parts[1]
    $fallback = ($parts[2] -replace ' - Google Chrome$', '').Trim()

    try {
        $ae = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$hwnd)
        $tabs = $ae.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tabCond)
        if ($tabs.Count -gt 0) {
            for ($i = 0; $i -lt $tabs.Count; $i++) {
                "$hwnd|$($i + 1)|$($tabs[$i].Current.Name)"
            }
        } else {
            "$hwnd|1|$fallback"
        }
    } catch {
        "$hwnd|1|$fallback"
    }
}
"#;

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut windows: std::collections::BTreeMap<String, Vec<BrowserTab>> =
        std::collections::BTreeMap::new();
    let mut first_pid: Option<u32> = None;

    // Collect PIDs per hwnd so we can populate BrowserInstance.pid
    let mut hwnd_pid: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();

    for line in stdout.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: hwnd|tabIndex|tabTitle  (note: pid removed, hwnd is first)
        // But we still get pid from parts[0] in the old C# output.
        // The new script outputs hwnd|index|title directly.
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() < 3 {
            continue;
        }

        let hwnd = parts[0].to_string();
        let tab_index: u32 = match parts[1].parse() {
            Ok(i) => i,
            Err(_) => continue,
        };
        let title = parts[2].trim().to_string();

        // Track first hwnd as pid proxy (we don't have pid in the new format,
        // but BrowserInstance.pid is optional and only used for display)
        if first_pid.is_none() {
            // Use hwnd as a stand-in; it's only informational
            first_pid = hwnd.parse().ok().map(|h: i64| h as u32);
        }
        hwnd_pid.entry(hwnd.clone()).or_insert(0);

        windows.entry(hwnd).or_default().push(BrowserTab {
            index: tab_index,
            uri: None,
            title,
            active: false,
        });
    }

    if windows.is_empty() {
        return Ok(None);
    }

    let browser_windows: Vec<BrowserWindow> = windows
        .into_iter()
        .map(|(id, tabs)| BrowserWindow { id, tabs })
        .collect();

    Ok(Some(BrowserInstance {
        app: "Google Chrome".to_string(),
        pid: first_pid,
        windows: browser_windows,
    }))
}

/// Focus a Chrome window and select a specific tab using UI Automation.
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
try {{ Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ZestfulWin32 {{ [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); }}' }} catch {{}}

$hwnd = [IntPtr]{hwnd}
[ZestfulWin32]::ShowWindow($hwnd, 9)
[ZestfulWin32]::SetForegroundWindow($hwnd)

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
    async fn test_focus_non_numeric_no_panic() {
        let result = focus("not-a-hwnd", None).await;
        assert!(result.is_ok());
    }
}
