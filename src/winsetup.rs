//! Optional Windows changes offered by Berth's "Recommended setup":
//! moving the taskbar to another screen edge, and showing or hiding the
//! "This PC" and "Recycle Bin" desktop icons. Nothing here runs without the
//! user choosing it, and every change can be undone (the previous values
//! are kept in settings.json).
//!
//! - The taskbar edge is the byte Windows keeps in
//!   `HKCU\...\Explorer\StuckRects3\Settings` (offset 12: 0 left, 1 top,
//!   2 right, 3 bottom). Explorer reads it when it starts, so Berth restarts
//!   Explorer with the Windows Restart Manager, then checks that the taskbar
//!   really moved. Windows versions without left/right/top taskbar support
//!   put it back at the bottom; Berth then restores the old value.
//! - Desktop icons are the documented per-user values under
//!   `HKCU\...\Explorer\HideDesktopIcons\NewStartPanel` (1 = hidden).

use std::time::{Duration, Instant};

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_BINARY, REG_DWORD,
    RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
};
use windows::Win32::System::RestartManager::{
    RmEndSession, RmForceShutdown, RmRegisterResources, RmRestart, RmShutdown, RmStartSession,
    CCH_RM_SESSION_KEY, RM_UNIQUE_PROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};

use crate::taskbar::{self, BarEdge};
use crate::util;

const STUCK_RECTS: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3";
const DESKTOP_ICONS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\HideDesktopIcons\NewStartPanel";
pub const THIS_PC: &str = "{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
pub const RECYCLE_BIN: &str = "{645FF040-5081-101B-9F08-00AA002F954E}";

pub fn edge_code(e: BarEdge) -> u8 {
    match e {
        BarEdge::Left => 0,
        BarEdge::Top => 1,
        BarEdge::Right => 2,
        BarEdge::Bottom => 3,
    }
}

fn read_stuck_rects() -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    let mut size = buf.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(STUCK_RECTS),
            &HSTRING::from("Settings"),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r.is_err() || size < 13 {
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

/// The taskbar edge Windows has stored (0 left, 1 top, 2 right, 3 bottom).
pub fn stored_edge() -> Option<u8> {
    read_stuck_rects().map(|b| b[12])
}

fn write_stored_edge(code: u8) -> Result<(), String> {
    let mut data = read_stuck_rects().ok_or("Windows' taskbar settings couldn't be read")?;
    data[12] = code;
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(STUCK_RECTS),
            &HSTRING::from("Settings"),
            REG_BINARY.0,
            Some(data.as_ptr() as *const _),
            data.len() as u32,
        )
    };
    if r.is_ok() {
        Ok(())
    } else {
        Err(format!("Windows returned error {}", r.0))
    }
}

/// Restarts Windows Explorer (the taskbar and desktop) through the Restart
/// Manager, and waits until the taskbar is back. `while_closed` runs after
/// Explorer has exited and before it starts again (Explorer saves its
/// taskbar settings when it exits, so changes must be written then).
pub fn restart_explorer(while_closed: impl FnOnce()) -> Result<(), String> {
    unsafe {
        let tray = FindWindowW(windows::core::w!("Shell_TrayWnd"), None)
            .map_err(|_| "the taskbar isn't running")?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(tray, Some(&mut pid));
        let proc_ = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| e.message().to_string())?;
        let (mut created, mut a, mut b, mut c) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let times = GetProcessTimes(proc_, &mut created, &mut a, &mut b, &mut c);
        let _ = CloseHandle(proc_);
        times.map_err(|e| e.message().to_string())?;

        util::log(&format!("restarting Explorer (pid {pid})"));
        let mut session = 0u32;
        let mut key = vec![0u16; CCH_RM_SESSION_KEY as usize + 1];
        let r = RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr()));
        if r.is_err() {
            return Err(format!(
                "the Restart Manager couldn't start (error {})",
                r.0
            ));
        }
        let procs = [RM_UNIQUE_PROCESS {
            dwProcessId: pid,
            ProcessStartTime: created,
        }];
        let mut result = Ok(());
        let r = RmRegisterResources(session, None, Some(&procs), None);
        if r.is_err() {
            result = Err(format!(
                "Explorer couldn't be registered for restart (error {})",
                r.0
            ));
        } else {
            let r = RmShutdown(session, RmForceShutdown.0 as u32, None);
            util::log(&format!("Explorer shutdown: {}", r.0));
            if r.is_err() {
                result = Err(format!("Explorer couldn't be closed (error {})", r.0));
            }
            // Make sure the old Explorer has exited before writing.
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, 5000);
                let _ = CloseHandle(h);
            }
            while_closed();
            let r = RmRestart(session, None, None);
            util::log(&format!("Explorer restart: {}", r.0));
        }
        let _ = RmEndSession(session);
        result?;
    }
    // Wait for the taskbar to come back; start Explorer ourselves if needed.
    let start = Instant::now();
    let mut launched = false;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let up = unsafe { FindWindowW(windows::core::w!("Shell_TrayWnd"), None).is_ok() };
        if up {
            // Give Explorer a moment to lay the taskbar out.
            std::thread::sleep(Duration::from_millis(1500));
            return Ok(());
        }
        if !launched && start.elapsed() > Duration::from_secs(5) {
            launched = true;
            util::log("Explorer didn't come back by itself; starting it");
            let _ = std::process::Command::new("explorer.exe").spawn();
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err("the taskbar didn't come back after restarting Explorer".into());
        }
    }
}

/// Outcome of moving the taskbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveResult {
    /// Already there.
    Unchanged,
    Moved,
    /// This Windows version put the taskbar back; the old setting was restored.
    NotSupported,
}

/// Moves the taskbar to `target` (blocking; run on a worker thread).
/// Returns the previous stored edge code too, for undo.
pub fn move_taskbar(target: BarEdge) -> Result<(MoveResult, Option<u8>), String> {
    if taskbar::main_edge() == Some(target) {
        return Ok((MoveResult::Unchanged, None));
    }
    let before = stored_edge().ok_or("Windows' taskbar settings couldn't be read")?;
    let mut wrote = Ok(());
    restart_explorer(|| wrote = write_stored_edge(edge_code(target)))?;
    wrote?;
    // Explorer may take a moment to apply the new position.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(4) {
        if taskbar::main_edge() == Some(target) {
            util::log(&format!("taskbar moved to {target:?}"));
            return Ok((MoveResult::Moved, Some(before)));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    util::log(&format!(
        "this Windows version didn't move the taskbar to {target:?}; restoring"
    ));
    if taskbar::main_edge().map(edge_code) != Some(before) {
        let mut wrote = Ok(());
        restart_explorer(|| wrote = write_stored_edge(before))?;
        wrote?;
    } else {
        write_stored_edge(before)?;
    }
    Ok((MoveResult::NotSupported, None))
}

/// Puts back a stored edge code saved earlier (blocking).
pub fn restore_taskbar_edge(code: u8) -> Result<(), String> {
    if stored_edge() == Some(code) && taskbar::main_edge().map(edge_code) == Some(code) {
        return Ok(());
    }
    let mut wrote = Ok(());
    restart_explorer(|| wrote = write_stored_edge(code))?;
    wrote
}

// ----- desktop icons -------------------------------------------------------------------

/// The raw "hidden" value for a desktop icon (None = not set, i.e. the
/// Windows default).
pub fn desktop_icon_value(clsid: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(DESKTOP_ICONS),
            &HSTRING::from(clsid),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(v)
}

/// Whether the icon is currently shown. Windows 11 shows Recycle Bin and
/// hides This PC by default.
pub fn desktop_icon_shown(clsid: &str) -> bool {
    match desktop_icon_value(clsid) {
        Some(v) => v == 0,
        None => clsid == RECYCLE_BIN,
    }
}

/// Sets (or, with None, removes) the raw value.
pub fn set_desktop_icon_value(clsid: &str, value: Option<u32>) -> Result<(), String> {
    let r = unsafe {
        match value {
            Some(v) => RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(DESKTOP_ICONS),
                &HSTRING::from(clsid),
                REG_DWORD.0,
                Some(&v as *const u32 as *const _),
                4,
            ),
            None => RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(DESKTOP_ICONS),
                &HSTRING::from(clsid),
            ),
        }
    };
    if r.is_ok() || (value.is_none() && r.0 == 2) {
        Ok(())
    } else {
        Err(format!("Windows returned error {}", r.0))
    }
}

/// Asks Explorer to redraw the desktop icons.
pub fn refresh_desktop() {
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, PostMessageW, WM_COMMAND};
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
        // The desktop's own "Refresh" command (the same as pressing F5 on it).
        let mut view = FindWindowW(w!("Progman"), None)
            .ok()
            .and_then(|p| FindWindowExW(Some(p), None, w!("SHELLDLL_DefView"), None).ok());
        if view.is_none() {
            let mut prev: Option<HWND> = None;
            while let Ok(worker) = FindWindowExW(None, prev, w!("WorkerW"), None) {
                if let Ok(v) = FindWindowExW(Some(worker), None, w!("SHELLDLL_DefView"), None) {
                    view = Some(v);
                    break;
                }
                prev = Some(worker);
            }
        }
        if let Some(v) = view {
            let _ = PostMessageW(Some(v), WM_COMMAND, WPARAM(0x7103), LPARAM(0));
        }
    }
}
