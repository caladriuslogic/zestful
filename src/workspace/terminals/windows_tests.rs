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
