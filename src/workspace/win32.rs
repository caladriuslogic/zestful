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

/// Find the window belonging to the specific editor instance that contains
/// `start_pid` in its process tree. Used for `workspace://code/window:<pid>`
/// URIs where the number is the Node.js PID of a VS Code worker process
/// (extension host, renderer, etc.), not a Win32 HWND.
///
/// `project_hint` (the project folder name from the URI's `project:` segment)
/// is used to disambiguate when a single Electron main process owns multiple
/// BrowserWindow HWNDs — the window whose title contains the hint wins.
///
/// Strategy:
/// 1. Collect ALL visible titled windows for all `exe_name` processes (not
///    deduplicated by PID — a single Electron main process can own several).
/// 2. Walk the parent-process chain from `start_pid`; at the first ancestor
///    that owns windows, pick the one matching `project_hint` or the first.
/// 3. If the chain never reaches a window-owner, match `project_hint` against
///    all window titles globally.
/// 4. Last resort: window whose owner PID is numerically nearest to `start_pid`.
pub fn find_ancestor_window(start_pid: u32, exe_name: &str, project_hint: Option<&str>) -> HWND {
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

    // Collect ALL visible titled windows — no dedup by PID so that a single
    // Electron main process with multiple BrowserWindows yields multiple entries.
    let all_windows = collect_titled_windows_for_pids(&target_pids);

    match all_windows.len() {
        0 => return 0,
        1 => return all_windows[0].1,
        _ => {}
    }

    // Build pid → Vec<(hwnd, title)> to handle multiple windows per PID.
    let mut pid_windows: HashMap<u32, Vec<(HWND, String)>> = HashMap::new();
    for &(pid, hwnd, ref title) in &all_windows {
        pid_windows.entry(pid).or_default().push((hwnd, title.clone()));
    }

    // Given a candidate list, pick the best HWND using project_hint.
    let pick = |candidates: &[(HWND, String)]| -> HWND {
        if candidates.len() == 1 {
            return candidates[0].0;
        }
        if let Some(hint) = project_hint {
            let hint_lc = hint.to_lowercase();
            if let Some(&(hwnd, _)) = candidates.iter().find(|(_, t)| t.to_lowercase().contains(&hint_lc)) {
                return hwnd;
            }
        }
        candidates[0].0 // frontmost in EnumWindows Z-order
    };

    // Strategy 1: walk the parent chain from start_pid.
    let mut cur = start_pid;
    for _ in 0..12 {
        if let Some(windows) = pid_windows.get(&cur) {
            return pick(windows);
        }
        match proc_map.get(&cur) {
            Some((ppid, _)) if *ppid != 0 && *ppid != cur => cur = *ppid,
            _ => break,
        }
    }

    // Strategy 2: ancestor chain didn't reach a window-owning process.
    // Match project_hint against all window titles.
    if let Some(hint) = project_hint {
        let hint_lc = hint.to_lowercase();
        if let Some(&(_, hwnd, _)) = all_windows.iter().find(|(_, _, t)| t.to_lowercase().contains(&hint_lc)) {
            return hwnd;
        }
    }

    // Final fallback: window-owning process with PID nearest to start_pid.
    all_windows
        .iter()
        .min_by_key(|(pid, _, _)| pid.abs_diff(start_pid))
        .map(|(_, hwnd, _)| *hwnd)
        .unwrap_or(0)
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
        // Only restore if minimized — SW_RESTORE on a maximized window un-maximizes it.
        if IsIconic(hwnd) != FALSE {
            ShowWindow(hwnd, SW_RESTORE);
            // If the window is still iconic after ShowWindow it is elevated and blocked by
            // UIPI. WM_SYSCOMMAND/SC_RESTORE is not in the UIPI block list, so PostMessageW
            // succeeds where ShowWindow's internal message dispatch does not.
            if IsIconic(hwnd) != FALSE {
                PostMessageW(hwnd, WM_SYSCOMMAND, SC_RESTORE as usize, 0);
            }
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

/// Collect ALL visible titled windows for the given PIDs — no deduplication.
/// Returns `(pid, hwnd, title)` triples in EnumWindows (front-to-back) order.
fn collect_titled_windows_for_pids(pids: &HashSet<u32>) -> Vec<(u32, HWND, String)> {
    struct State {
        pids: *const HashSet<u32>,
        results: Vec<(u32, HWND, String)>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam as *mut State);
        if IsWindowVisible(hwnd) == FALSE {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if !(*state.pids).contains(&pid) {
            return TRUE;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if len <= 0 {
            return TRUE;
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        (*state).results.push((pid, hwnd, title));
        TRUE
    }
    let mut state = State { pids: pids as *const _, results: Vec::new() };
    unsafe {
        EnumWindows(Some(cb), &mut state as *mut _ as LPARAM);
    }
    state.results
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

/// Find the visible CASCADIA_HOSTING_WINDOW_CLASS frame window owned by `wt_pid`.
pub fn find_cascadia_frame_for_pid(wt_pid: u32) -> HWND {
    struct State { wt_pid: u32, result: HWND }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam as *mut State);
        if IsWindowVisible(hwnd) == FALSE { return TRUE; }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != state.wt_pid { return TRUE; }
        let mut cls = [0u16; 64];
        GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32);
        if wcs_to_string(&cls) != "CASCADIA_HOSTING_WINDOW_CLASS" { return TRUE; }
        state.result = hwnd;
        FALSE
    }
    let mut state = State { wt_pid, result: 0 };
    unsafe { EnumWindows(Some(cb), &mut state as *mut _ as LPARAM); }
    state.result
}

/// Return the shell PID for each PseudoConsoleWindow whose Win32 parent is `frame_hwnd`.
pub fn find_pseudo_console_shell_pids(frame_hwnd: HWND) -> Vec<u32> {
    struct State { frame_hwnd: HWND, results: Vec<u32> }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam as *mut State);
        if GetParent(hwnd) != state.frame_hwnd { return TRUE; }
        let mut cls = [0u16; 64];
        GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32);
        if wcs_to_string(&cls) != "PseudoConsoleWindow" { return TRUE; }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        (*state).results.push(pid);
        TRUE
    }
    let mut state = State { frame_hwnd, results: Vec::new() };
    unsafe { EnumWindows(Some(cb), &mut state as *mut _ as LPARAM); }
    state.results
}

fn wcs_to_string(wcs: &[u16]) -> String {
    let end = wcs.iter().position(|&c| c == 0).unwrap_or(wcs.len());
    String::from_utf16_lossy(&wcs[..end])
}
