//! Direct Win32 API wrappers for process enumeration and window focus.
//!
//! Replaces PowerShell subprocesses and runtime C# compilation (Add-Type)
//! for all process-query and window-focus operations on Windows.

use std::collections::{HashMap, HashSet};

use windows_sys::Win32::Foundation::{
    CloseHandle, BOOL, FALSE, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, TRUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{AttachThreadInput, OpenProcess, PROCESS_SYNCHRONIZE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow, GetParent, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, SC_RESTORE,
    SetForegroundWindow, ShowWindow, SW_RESTORE, WM_SYSCOMMAND,
};

/// Snapshot all running processes.
/// Returns map of pid → (parent_pid, exe_name_lowercase).
pub fn snapshot_processes() -> HashMap<u32, (u32, String)> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return map;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != FALSE {
            loop {
                let exe = wcs_to_string(&entry.szExeFile).to_lowercase();
                map.insert(entry.th32ProcessID, (entry.th32ParentProcessID, exe));
                if Process32NextW(snap, &mut entry) == FALSE {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    map
}

/// Find all PIDs whose executable name matches (case-insensitive, .exe suffix optional).
pub fn find_pids_by_exe(exe_name: &str) -> Vec<u32> {
    let target = exe_name.to_lowercase();
    let target = if target.ends_with(".exe") {
        target
    } else {
        format!("{}.exe", target)
    };
    snapshot_processes()
        .into_iter()
        .filter(|(_, (_, exe))| *exe == target)
        .map(|(pid, _)| pid)
        .collect()
}

/// Return the first PID for a named executable, or None.
pub fn first_pid_by_exe(exe_name: &str) -> Option<u32> {
    find_pids_by_exe(exe_name).into_iter().next()
}

/// Check whether a process with the given PID is alive by opening a handle.
pub fn is_process_alive(pid: u32) -> bool {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_SYNCHRONIZE, FALSE, pid);
        if h == 0 {
            return false;
        }
        CloseHandle(h);
        true
    }
}

/// Enumerate visible top-level windows for the given executable and return
/// (pid, window_title) pairs. Processes with no visible titled window are
/// excluded — equivalent to tasklist's `WINDOWTITLE ne N/A` filter.
///
/// Classic console processes (cmd.exe, powershell.exe) don't own their own
/// top-level window — conhost.exe (a child process) hosts the visible console
/// window instead. This function includes conhost.exe children in the window
/// search and re-attributes any matches back to the shell parent PID.
pub fn query_processes(exe_name: &str) -> Vec<(u32, String)> {
    let target = {
        let t = exe_name.to_lowercase();
        if t.ends_with(".exe") {
            t
        } else {
            format!("{}.exe", t)
        }
    };

    let proc_map = snapshot_processes();

    let target_pids: HashSet<u32> = proc_map
        .iter()
        .filter(|(_, (_, exe))| *exe == target)
        .map(|(pid, _)| *pid)
        .collect();

    if target_pids.is_empty() {
        return vec![];
    }

    // On Windows 10/11, classic console windows are hosted by a conhost.exe child,
    // not the shell itself. Map conhost_pid → shell_pid so we can search for those
    // windows and attribute them back to the correct process.
    let conhost_to_shell: HashMap<u32, u32> = proc_map
        .iter()
        .filter_map(|(&child_pid, (ppid, exe))| {
            if exe == "conhost.exe" && target_pids.contains(ppid) {
                Some((child_pid, *ppid))
            } else {
                None
            }
        })
        .collect();

    let search_pids: HashSet<u32> = target_pids
        .iter()
        .copied()
        .chain(conhost_to_shell.keys().copied())
        .collect();

    collect_windows_for_pids(&search_pids)
        .into_iter()
        .map(|(pid, title)| (conhost_to_shell.get(&pid).copied().unwrap_or(pid), title))
        .collect()
}

/// Focus the window belonging to the given PID using three strategies:
///
/// 1. Direct window or conhost.exe child — standalone console on Win10/11.
/// 2. Parent-chain walk for windowsterminal.exe/openconsole.exe — WT "New Tab".
/// 3. Any visible windowsterminal.exe window — Win11 defterm interception.
///
/// AttachThreadInput bypasses the Windows 11 foreground lock.
pub fn focus_by_pid(pid: u32) {
    let proc_map = snapshot_processes();

    // Candidate PIDs: the target process + its conhost.exe children.
    let mut candidate_pids: HashSet<u32> = HashSet::new();
    candidate_pids.insert(pid);
    for (&child_pid, (ppid, exe)) in &proc_map {
        if *ppid == pid && exe == "conhost.exe" {
            candidate_pids.insert(child_pid);
        }
    }

    // Strategy 1a: direct visible window or conhost.exe child.
    let mut hwnd = find_visible_window(&candidate_pids);

    // Strategy 1b: PseudoConsoleWindow (WT-hosted process) → raise the WT frame.
    // PseudoConsoleWindow is not visible, so Strategy 1a misses it. Its Win32 parent
    // (GetParent) is the CASCADIA_HOSTING_WINDOW_CLASS frame that can be raised.
    if hwnd == 0 {
        hwnd = find_wt_frame_for_pids(&candidate_pids);
    }

    // Strategy 2: walk parent chain.
    if hwnd == 0 {
        let mut cur = pid;
        'walk: for _ in 0..4 {
            match proc_map.get(&cur) {
                Some((ppid, _)) if *ppid != 0 => {
                    cur = *ppid;
                    if let Some((_, exe)) = proc_map.get(&cur) {
                        if exe == "windowsterminal.exe" || exe == "openconsole.exe" {
                            let mut s = HashSet::new();
                            s.insert(cur);
                            hwnd = find_visible_window(&s);
                            if hwnd != 0 {
                                break 'walk;
                            }
                        }
                    }
                }
                _ => break,
            }
        }
    }

    // Strategy 3: any windowsterminal.exe window.
    if hwnd == 0 {
        let wt_pids: HashSet<u32> = proc_map
            .iter()
            .filter(|(_, (_, exe))| exe == "windowsterminal.exe")
            .map(|(pid, _)| *pid)
            .collect();
        if !wt_pids.is_empty() {
            hwnd = find_visible_window(&wt_pids);
        }
    }

    if hwnd != 0 {
        raise_window(hwnd);
    }
}

/// Walk the parent-process chain starting at `start_pid`, checking at each
/// Code.exe ancestor whether it owns a visible top-level window. Returns the
/// first such HWND, or — if none is found along the chain — the window owned
/// by any Code.exe process as a last-resort fallback.
///
/// This is the right primitive for `workspace://code/window:<pid>` URIs where
/// the embedded number is a Node.js process PID (extension host or renderer),
/// not a Win32 HWND.
pub fn find_ancestor_window(start_pid: u32, exe_name: &str) -> HWND {
    let target = {
        let t = exe_name.to_lowercase();
        if t.ends_with(".exe") { t } else { format!("{}.exe", t) }
    };
    let proc_map = snapshot_processes();

    let target_pids: HashSet<u32> = proc_map
        .iter()
        .filter(|(_, (_, exe))| *exe == target)
        .map(|(pid, _)| *pid)
        .collect();

    // Walk up from start_pid; at each Code.exe ancestor try to find its window.
    let mut cur = start_pid;
    for _ in 0..8 {
        if target_pids.contains(&cur) {
            let mut s = HashSet::new();
            s.insert(cur);
            let hwnd = find_visible_window(&s);
            if hwnd != 0 {
                return hwnd;
            }
        }
        match proc_map.get(&cur) {
            Some((ppid, _)) if *ppid != 0 && *ppid != cur => cur = *ppid,
            _ => break,
        }
    }

    // Fallback: any visible window owned by any Code.exe process.
    find_visible_window(&target_pids)
}

/// Bring a window to the foreground.
/// Uses AttachThreadInput to bypass the Windows 11 foreground lock.
pub fn raise_window(hwnd: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let mut dummy: u32 = 0;
        let fg_thread = GetWindowThreadProcessId(fg, &mut dummy);
        let tgt_thread = GetWindowThreadProcessId(hwnd, &mut dummy);
        let threads_differ = fg_thread != tgt_thread && fg_thread != 0;
        if threads_differ {
            AttachThreadInput(fg_thread, tgt_thread, TRUE);
        }
        ShowWindow(hwnd, SW_RESTORE);
        // ShowWindow can silently fail on elevated (admin) windows when called from a
        // non-elevated process. WM_SYSCOMMAND/SC_RESTORE is not in the UIPI block list,
        // so PostMessageW succeeds where ShowWindow's internal message dispatch does not.
        if IsIconic(hwnd) != FALSE {
            PostMessageW(hwnd, WM_SYSCOMMAND, SC_RESTORE as usize, 0);
        }
        SetForegroundWindow(hwnd);
        BringWindowToTop(hwnd);
        if threads_differ {
            AttachThreadInput(fg_thread, tgt_thread, FALSE);
        }
    }
}

// ── internal helpers ───────────────────────────────────────────────────────────

struct FindWindowState {
    pids: *const HashSet<u32>,
    result: HWND,
}

unsafe extern "system" fn find_window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam as *mut FindWindowState);
    if IsWindowVisible(hwnd) == FALSE {
        return TRUE;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if (*state.pids).contains(&pid) {
        state.result = hwnd;
        return FALSE; // stop enumeration
    }
    TRUE
}

fn find_visible_window(pids: &HashSet<u32>) -> HWND {
    let mut state = FindWindowState {
        pids: pids as *const _,
        result: 0,
    };
    unsafe {
        EnumWindows(Some(find_window_cb), &mut state as *mut _ as LPARAM);
    }
    state.result
}

struct CollectState {
    pids: *const HashSet<u32>,
    seen: HashMap<u32, String>,
    pseudo_pids: HashSet<u32>,
}

unsafe extern "system" fn collect_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam as *mut CollectState);
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if !(*state.pids).contains(&pid) || state.seen.contains_key(&pid) {
        return TRUE;
    }
    let mut cls = [0u16; 128];
    GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32);
    let cls_str = wcs_to_string(&cls);

    // PseudoConsoleWindow is invisible but indicates a WT-hosted interactive console.
    // Track it separately so it doesn't shadow a visible titled window for the same PID
    // (conhost.exe on modern Windows can own both).
    if cls_str == "PseudoConsoleWindow" {
        state.pseudo_pids.insert(pid);
        return TRUE;
    }
    if IsWindowVisible(hwnd) == FALSE {
        return TRUE;
    }
    // Skip COM message-only windows.
    if cls_str == "OleMainThreadWndName" {
        return TRUE;
    }
    let mut buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if len <= 0 {
        return TRUE;
    }
    let title = String::from_utf16_lossy(&buf[..len as usize]);
    state.seen.insert(pid, title);
    TRUE
}

/// For processes running inside Windows Terminal, find the WT frame window (CASCADIA_HOSTING_WINDOW_CLASS)
/// by locating the PseudoConsoleWindow whose GetWindowThreadProcessId matches one of the given PIDs,
/// then returning that window's Win32 parent (the WT frame).
fn find_wt_frame_for_pids(pids: &HashSet<u32>) -> HWND {
    struct State {
        pids: *const HashSet<u32>,
        result: HWND,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam as *mut State);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if !(*state.pids).contains(&pid) {
            return TRUE;
        }
        let mut cls = [0u16; 64];
        GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32);
        if wcs_to_string(&cls) != "PseudoConsoleWindow" {
            return TRUE;
        }
        let parent = GetParent(hwnd);
        if parent != 0 {
            state.result = parent;
            return FALSE; // stop enumeration
        }
        TRUE
    }
    let mut state = State {
        pids: pids as *const _,
        result: 0,
    };
    unsafe {
        EnumWindows(Some(cb), &mut state as *mut _ as LPARAM);
    }
    state.result
}

fn collect_windows_for_pids(pids: &HashSet<u32>) -> Vec<(u32, String)> {
    let mut state = CollectState {
        pids: pids as *const _,
        seen: HashMap::new(),
        pseudo_pids: HashSet::new(),
    };
    unsafe {
        EnumWindows(Some(collect_cb), &mut state as *mut _ as LPARAM);
    }
    // PIDs only reachable via PseudoConsoleWindow (WT-hosted, no visible window of their own)
    // get an empty title so they still appear in the result set.
    for pid in state.pseudo_pids {
        state.seen.entry(pid).or_insert_with(String::new);
    }
    state.seen.into_iter().collect()
}

fn wcs_to_string(wcs: &[u16]) -> String {
    let end = wcs.iter().position(|&c| c == 0).unwrap_or(wcs.len());
    String::from_utf16_lossy(&wcs[..end])
}
