//! Google Chrome tab detection via AppleScript.

use anyhow::Result;
use std::process::Command;

use crate::workspace::types::{BrowserInstance, BrowserTab, BrowserWindow};

pub fn detect() -> Result<Option<BrowserInstance>> {
    let pid = get_chrome_pid();
    if pid.is_none() {
        return Ok(None);
    }

    let (active_win, active_tab) = get_active_tab().unwrap_or((String::new(), 0));

    let output = Command::new("osascript")
        .args([
            "-e",
            r#"tell application "Google Chrome"
  set output to ""
  repeat with w in windows
    set wId to id of w
    set tabList to tabs of w
    repeat with i from 1 to count of tabList
      set t to item i of tabList
      set output to output & (wId as text) & "	" & (i as text) & "	" & title of t & linefeed
    end repeat
  end repeat
  return output
end tell"#,
        ])
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut windows: Vec<BrowserWindow> = Vec::new();

    for line in stdout.trim().lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let win_id = parts[0].to_string();
        let tab_index: u32 = parts[1].parse().unwrap_or(0);
        let title = parts[2].to_string();
        let is_active = win_id == active_win && tab_index == active_tab;

        let tab = BrowserTab {
            index: tab_index,
            uri: None,
            title,
            active: is_active,
        };

        if let Some(win) = windows.iter_mut().find(|w| w.id == win_id) {
            win.tabs.push(tab);
        } else {
            windows.push(BrowserWindow {
                id: win_id,
                tabs: vec![tab],
            });
        }
    }

    Ok(Some(BrowserInstance {
        app: "Google Chrome".to_string(),
        pid,
        windows,
    }))
}

/// Focus a Chrome tab. The `tab_id` can be either a small 1-based tab index
/// (legacy) or the Chrome tab's unique id (matches `chrome.tabs.Tab.id` from
/// the JS API). Values ≤ 1000 are treated as indices; larger values are
/// treated as Chrome tab IDs and looked up within the window.
pub async fn focus(window_id: &str, tab_id: u64) -> anyhow::Result<()> {
    let win_id: i64 = window_id.parse().unwrap_or(-1);
    let script = if tab_id <= 1000 {
        // Small number: treat as 1-based tab index
        format!(
            r#"tell application "Google Chrome"
  try
    set w to window id {}
    set active tab index of w to {}
    set index of w to 1
    activate
  on error
  end try
end tell"#,
            win_id, tab_id
        )
    } else {
        // Large number: look up tab by its unique id (AppleScript returns it as text) within the window
        format!(
            r#"tell application "Google Chrome"
  try
    set w to window id {}
    set tabList to tabs of w
    repeat with i from 1 to count of tabList
      if (id of (item i of tabList) as text) is "{}" then
        set active tab index of w to i
        exit repeat
      end if
    end repeat
    set index of w to 1
    activate
  on error
  end try
end tell"#,
            win_id, tab_id
        )
    };

    tokio::task::spawn_blocking(move || {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output();
    })
    .await?;

    Ok(())
}

fn get_chrome_pid() -> Option<u32> {
    let output = Command::new("pgrep")
        .args(["-x", "Google Chrome"])
        .output()
        .ok()?;

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

fn get_active_tab() -> Option<(String, u32)> {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"tell application "Google Chrome"
  return (id of front window as text) & "	" & (active tab index of front window as text)
end tell"#,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('\t').collect();
    if parts.len() < 2 {
        return None;
    }

    let win_id = parts[0].to_string();
    let tab_index: u32 = parts[1].parse().ok()?;
    Some((win_id, tab_index))
}
