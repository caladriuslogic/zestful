//! Integration tests for Windows terminal detection and focus.
//!
//! Each test spawns one or more real console windows and verifies that the
//! detectors and focus handlers behave correctly against live processes.
//!
//! The tests are marked `#[ignore]` because they open visible windows and
//! require an interactive desktop session.  Run them with:
//!
//!   cargo test -- --ignored --nocapture

use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::Duration;

/// `CREATE_NEW_CONSOLE` — spawns the child in its own visible console window.
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// RAII guard: kills and reaps the child process when dropped so that test
/// failures don't leave stray windows behind.
struct TermGuard(Child);

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_cmd() -> (u32, TermGuard) {
    let child = Command::new("cmd.exe")
        .args(["/k"])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("failed to spawn cmd.exe");
    let pid = child.id();
    (pid, TermGuard(child))
}

fn spawn_powershell() -> (u32, TermGuard) {
    let child = Command::new("powershell.exe")
        .args(["-NoExit", "-Command", "$null"])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("failed to spawn powershell.exe");
    let pid = child.id();
    (pid, TermGuard(child))
}

/// Wait long enough for the new window to register in tasklist.
fn wait_for_window() {
    std::thread::sleep(Duration::from_millis(500));
}

// ── Detection tests ───────────────────────────────────────────────────────────

#[test]
#[ignore = "opens a visible cmd.exe window; run with: cargo test -- --ignored"]
fn detect_finds_cmd_window() {
    let (pid, _guard) = spawn_cmd();
    wait_for_window();

    let terminal = super::cmd::detect()
        .expect("cmd::detect() returned Err")
        .expect("cmd::detect() returned None — no cmd.exe detected");

    let found = terminal
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter())
        .any(|t| t.shell_pid == Some(pid));

    assert!(
        found,
        "spawned cmd.exe (pid {pid}) not found in detect() output\nGot: {terminal:?}"
    );
}

#[test]
#[ignore = "opens a visible powershell.exe window; run with: cargo test -- --ignored"]
fn detect_finds_powershell_window() {
    let (pid, _guard) = spawn_powershell();
    wait_for_window();

    let terminal = super::powershell::detect()
        .expect("powershell::detect() returned Err")
        .expect("powershell::detect() returned None — no powershell.exe detected");

    let found = terminal
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter())
        .any(|t| t.shell_pid == Some(pid));

    assert!(
        found,
        "spawned powershell.exe (pid {pid}) not found in detect() output\nGot: {terminal:?}"
    );
}

#[test]
#[ignore = "opens both cmd.exe and powershell.exe windows; run with: cargo test -- --ignored"]
fn detectors_do_not_cross_report() {
    let (cmd_pid, _cmd) = spawn_cmd();
    let (ps_pid, _ps) = spawn_powershell();
    wait_for_window();

    // cmd detector must not include the powershell PID.
    if let Ok(Some(t)) = super::cmd::detect() {
        let found_ps = t
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .any(|tab| tab.shell_pid == Some(ps_pid));
        assert!(!found_ps, "cmd::detect() reported powershell.exe pid {ps_pid}");
    }

    // powershell detector must not include the cmd PID.
    if let Ok(Some(t)) = super::powershell::detect() {
        let found_cmd = t
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .any(|tab| tab.shell_pid == Some(cmd_pid));
        assert!(!found_cmd, "powershell::detect() reported cmd.exe pid {cmd_pid}");
    }
}

#[test]
#[ignore = "opens cmd.exe; verifies non-interactive subprocesses are excluded; run with: cargo test -- --ignored"]
fn no_false_positives_from_background_cmd() {
    // Spawn a non-interactive subprocess that inherits the parent's (hidden)
    // console — it should appear in tasklist with WINDOWTITLE=N/A and be
    // filtered out by query_tasklist.
    let _background = Command::new("cmd.exe")
        .args(["/c", "ping", "-n", "5", "127.0.0.1"])
        .spawn() // no CREATE_NEW_CONSOLE → no visible window → N/A title
        .expect("failed to spawn background cmd /c");

    // Also open one real interactive window so we know detect() is working.
    let (pid, _guard) = spawn_cmd();
    wait_for_window();

    let terminal = super::cmd::detect()
        .expect("cmd::detect() returned Err")
        .expect("cmd::detect() returned None");

    // The interactive window must be present.
    let found = terminal
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter())
        .any(|t| t.shell_pid == Some(pid));
    assert!(found, "interactive cmd.exe pid={pid} was not detected");

    // Every detected tab must have a shell_pid — a tab without one indicates
    // a ghost entry that slipped past the WINDOWTITLE filter.
    for win in &terminal.windows {
        for tab in &win.tabs {
            assert!(
                tab.shell_pid.is_some(),
                "detected tab '{title}' has no shell_pid (possible false positive)",
                title = tab.title,
            );
        }
    }
}

// ── Windows Terminal helpers ──────────────────────────────────────────────────

/// Mutex that serializes all Windows Terminal tests.
///
/// `cargo test` runs tests on multiple threads by default.  The WT tests
/// share a single WT window (via `wt -w 0 new-tab`) and share cleanup
/// responsibilities, so parallel execution causes guards to kill each
/// other's shells.  Holding this lock for the lifetime of each WT test
/// ensures they run one at a time.
static WT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Returns true if `wt.exe` is available on this system.
fn wt_available() -> bool {
    // Check the standard MSIX install location first.
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let path = std::path::Path::new(&local)
            .join("Microsoft")
            .join("WindowsApps")
            .join("wt.exe");
        if path.exists() {
            return true;
        }
    }
    // Fall back to a PATH search.
    Command::new("where")
        .arg("wt.exe")
        .output()
        .map_or(false, |o| o.status.success())
}

/// Returns the set of shell PIDs currently hosted in any Windows Terminal window.
fn wt_shell_pids_snapshot() -> std::collections::HashSet<u32> {
    super::windows_terminal::detect()
        .ok()
        .flatten()
        .map(|t| {
            t.windows
                .iter()
                .flat_map(|w| w.tabs.iter())
                .filter_map(|tab| tab.shell_pid)
                .collect()
        })
        .unwrap_or_default()
}

/// RAII guard that cleans up all processes opened by the test.
///
/// Strategy:
///   1. Kill every hosted shell PID so the shells cannot become orphaned
///      standalone console windows.
///   2. Sleep briefly so WT has time to reap the exited shells.
///   3. Kill any `WindowsTerminal.exe` processes that were newly created by
///      the test (identified by a before/after PID snapshot).
///   4. If no new WT processes were created (the test added tabs to the
///      user's *existing* WT window), close those specific frame windows by
///      deriving their owner PID from the stored HWNDs.  This handles the
///      common case of running tests from inside a WT session.
struct WtGuard {
    shell_pids: Vec<u32>,
    /// WT process PIDs that were freshly spawned for this test.
    wt_pids: Vec<u32>,
    /// WT frame window HWNDs that received tabs during this test.
    frame_hwnds: Vec<i64>,
}

impl Drop for WtGuard {
    fn drop(&mut self) {
        // Step 1 — kill hosted shells.
        for pid in &self.shell_pids {
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }

        // Step 2 — brief pause for WT to detect the shell exits.
        std::thread::sleep(Duration::from_millis(800));

        // Step 3 — kill any WT processes we explicitly spawned.
        for pid in &self.wt_pids {
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }

        // Step 4 — close any WT windows that still host our HWNDs.
        // This fires when the test added tabs to a pre-existing WT window
        // (so wt_pids is empty) or when Step 3 was a no-op.
        if self.frame_hwnds.is_empty() {
            return;
        }
        let hwnd_list = self
            .frame_hwnds
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            r#"
try {{ Add-Type -TypeDefinition '
using System; using System.Runtime.InteropServices;
public class ZestfulWtClean {{
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
}}' -ErrorAction SilentlyContinue }} catch {{}}
foreach ($hwnd in @({hwnd_list})) {{
    $ptr = [IntPtr][long]$hwnd
    if ([ZestfulWtClean]::IsWindow($ptr)) {{
        $p = [uint32]0
        [ZestfulWtClean]::GetWindowThreadProcessId($ptr, [ref]$p) | Out-Null
        if ($p -gt 0) {{ & taskkill /F /PID $p 2>$null }}
    }}
}}"#,
            hwnd_list = hwnd_list
        );
        let _ = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output();
    }
}

/// Returns the PIDs of all currently running Windows Terminal processes.
/// Covers both the stable release (`WindowsTerminal.exe`) and the preview
/// build (`WindowsTerminalPreview.exe`).
fn wt_process_pids() -> std::collections::HashSet<u32> {
    let mut pids = std::collections::HashSet::new();
    for name in &["WindowsTerminal", "WindowsTerminalPreview"] {
        for pid in crate::workspace::process::find_pids_by_name(name) {
            pids.insert(pid);
        }
    }
    pids
}

/// Returns the shell args for the Nth tab (0-based), alternating between
/// cmd.exe (even) and powershell.exe (odd).
fn tab_shell_args(index: usize) -> Vec<&'static str> {
    if index % 2 == 0 {
        vec!["cmd.exe", "/k"]
    } else {
        vec!["powershell.exe", "-NoExit", "-Command", "$null"]
    }
}

/// Opens Windows Terminal with `tab_count` tabs, alternating cmd.exe / powershell.exe.
/// Returns `(hwnd, shell_pid)` pairs for the new tabs plus a cleanup guard.
///
/// Uses a before/after snapshot of detected shell PIDs to identify the tabs
/// that belong to this test, so existing WT windows are not disturbed.
/// Uses a before/after snapshot of WindowsTerminal.exe PIDs to identify which
/// WT processes were spawned by this test, so only those windows are closed on cleanup.
fn open_wt_tabs(tab_count: usize) -> (Vec<(String, u32)>, WtGuard) {
    assert!(tab_count >= 1);

    let shell_before = wt_shell_pids_snapshot();
    let wt_before = wt_process_pids();

    // Open the first tab in a new WT instance, then add subsequent tabs
    // via `wt -w 0 new-tab` which explicitly targets the last-used WT
    // window.  The semicolon syntax proved unreliable — some WT versions
    // treat each `; new-tab` as a separate window invocation.
    Command::new("wt.exe")
        .args(tab_shell_args(0))
        .spawn()
        .expect("failed to spawn wt.exe for first tab");

    // Give WT time to fully start before adding more tabs.
    std::thread::sleep(Duration::from_millis(2000));

    for i in 1..tab_count {
        let mut args = vec!["-w", "0", "new-tab"];
        args.extend_from_slice(&tab_shell_args(i));
        Command::new("wt.exe")
            .args(&args)
            .spawn()
            .expect("failed to add WT tab via -w 0 new-tab");
        // Brief pause between tabs so WT registers each one.
        std::thread::sleep(Duration::from_millis(1000));
    }

    // Final settle time.
    std::thread::sleep(Duration::from_millis(500));

    let wt_after = wt_process_pids();

    let terminal = super::windows_terminal::detect()
        .expect("windows_terminal::detect() returned Err")
        .expect("windows_terminal::detect() returned None — WT not detected after launch");

    // New tabs are those whose shell_pid was absent in the before-snapshot.
    let new_tabs: Vec<(String, u32)> = terminal
        .windows
        .iter()
        .flat_map(|w| {
            let hwnd = w.id.clone();
            w.tabs
                .iter()
                .filter_map(move |t| t.shell_pid.map(|pid| (hwnd.clone(), pid)))
        })
        .filter(|(_, pid)| !shell_before.contains(pid))
        .collect();

    // New WT processes are those absent in the before-snapshot.
    let new_wt_pids: Vec<u32> = wt_after.difference(&wt_before).copied().collect();

    // Unique WT frame HWNDs that received our tabs — used as a fallback to
    // close the window when wt_pids is empty (tabs added to existing WT).
    let frame_hwnds: Vec<i64> = new_tabs
        .iter()
        .map(|(h, _)| h.parse::<i64>().unwrap_or(0))
        .filter(|&h| h != 0)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let guard = WtGuard {
        shell_pids: new_tabs.iter().map(|(_, pid)| *pid).collect(),
        wt_pids: new_wt_pids,
        frame_hwnds,
    };

    (new_tabs, guard)
}

// ── Windows Terminal detection tests ─────────────────────────────────────────

#[test]
#[ignore = "opens Windows Terminal with 2 tabs; run with: cargo test -- --ignored"]
fn detect_wt_two_tabs() {
    let _wt_lock = WT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !wt_available() {
        eprintln!("wt.exe not found — skipping");
        return;
    }

    let (new_tabs, _guard) = open_wt_tabs(2);

    assert!(
        new_tabs.len() >= 2,
        "expected ≥2 new WT tabs, detected {} new tab(s): {:?}",
        new_tabs.len(),
        new_tabs,
    );

    // Both tabs must be in the same WT window (same HWND).
    let hwnds: std::collections::HashSet<&str> =
        new_tabs.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(
        hwnds.len(),
        1,
        "new tabs were spread across multiple WT windows: {:?}",
        hwnds,
    );

    println!("WT window {} — {} new tab(s):", new_tabs[0].0, new_tabs.len());
    for (i, (hwnd, pid)) in new_tabs.iter().enumerate() {
        println!("  Tab {}: hwnd={hwnd} shell_pid={pid}", i + 1);
    }
}

#[test]
#[ignore = "opens Windows Terminal with 3 tabs; run with: cargo test -- --ignored"]
fn detect_wt_three_tabs() {
    let _wt_lock = WT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !wt_available() {
        eprintln!("wt.exe not found — skipping");
        return;
    }

    let (new_tabs, _guard) = open_wt_tabs(3);

    assert!(
        new_tabs.len() >= 3,
        "expected ≥3 new WT tabs, detected {} new tab(s): {:?}",
        new_tabs.len(),
        new_tabs,
    );

    let hwnds: std::collections::HashSet<&str> =
        new_tabs.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(hwnds.len(), 1, "tabs spread across windows: {:?}", hwnds);

    println!("WT window {} — {} new tab(s):", new_tabs[0].0, new_tabs.len());
    for (i, (_, pid)) in new_tabs.iter().enumerate() {
        println!("  Tab {}: shell_pid={pid}", i + 1);
    }
}

#[test]
#[ignore = "opens Windows Terminal; verifies each tab has a shell_pid; run with: cargo test -- --ignored"]
fn detect_wt_tabs_have_shell_pids() {
    let _wt_lock = WT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !wt_available() {
        eprintln!("wt.exe not found — skipping");
        return;
    }

    let (new_tabs, _guard) = open_wt_tabs(2);

    assert!(
        !new_tabs.is_empty(),
        "no new WT tabs detected after opening WT"
    );

    for (i, (_, pid)) in new_tabs.iter().enumerate() {
        assert!(*pid > 0, "tab {} has shell_pid=0", i + 1);
    }
}

// ── Windows Terminal focus tests ──────────────────────────────────────────────

#[tokio::test]
#[ignore = "opens Windows Terminal with 2 tabs and cycles focus; run with: cargo test -- --ignored"]
async fn focus_wt_two_tabs() {
    let _wt_lock = WT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !wt_available() {
        eprintln!("wt.exe not found — skipping");
        return;
    }

    let (new_tabs, _guard) = open_wt_tabs(2);

    assert!(
        new_tabs.len() >= 2,
        "expected ≥2 new WT tabs to focus, got {}: {:?}",
        new_tabs.len(),
        new_tabs,
    );

    // Forward pass then backward pass — exercises switching in both directions.
    let order: Vec<usize> = (0..new_tabs.len())
        .chain((0..new_tabs.len()).rev())
        .collect();

    for idx in order {
        let (hwnd, shell_pid) = &new_tabs[idx];
        println!("Focusing WT hwnd={hwnd} shell_pid={shell_pid}");
        super::windows_terminal::focus(hwnd, Some(&shell_pid.to_string()))
            .await
            .expect("windows_terminal::focus() returned Err");
        std::thread::sleep(Duration::from_millis(500));
    }
}

// ── Focus tests ───────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "opens a cmd.exe window and brings it to the foreground; run with: cargo test -- --ignored"]
async fn focus_cmd_window() {
    let (pid, _guard) = spawn_cmd();
    wait_for_window();

    super::cmd::focus(Some(&pid.to_string()))
        .await
        .expect("cmd::focus() returned Err");
}

#[tokio::test]
#[ignore = "opens a powershell.exe window and brings it to the foreground; run with: cargo test -- --ignored"]
async fn focus_powershell_window() {
    let (pid, _guard) = spawn_powershell();
    wait_for_window();

    super::powershell::focus(Some(&pid.to_string()))
        .await
        .expect("powershell::focus() returned Err");
}

#[tokio::test]
#[ignore = "opens cmd.exe and powershell.exe and cycles focus between them; run with: cargo test -- --ignored"]
async fn focus_cycles_between_terminals() {
    let (cmd_pid, _cmd) = spawn_cmd();
    let (ps_pid, _ps) = spawn_powershell();
    wait_for_window();

    // Focus cmd, then powershell, then cmd again.
    super::cmd::focus(Some(&cmd_pid.to_string()))
        .await
        .expect("cmd::focus() pass 1 returned Err");

    std::thread::sleep(Duration::from_millis(300));

    super::powershell::focus(Some(&ps_pid.to_string()))
        .await
        .expect("powershell::focus() returned Err");

    std::thread::sleep(Duration::from_millis(300));

    super::cmd::focus(Some(&cmd_pid.to_string()))
        .await
        .expect("cmd::focus() pass 2 returned Err");
}
