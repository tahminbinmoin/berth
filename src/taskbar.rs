//! Optional "hide the Windows taskbar while Berth is running".
//!
//! This turns on Windows' own taskbar auto-hide (the same switch as
//! Settings > Personalization > Taskbar > Taskbar behaviors > Automatically
//! hide the taskbar) through the documented `SHAppBarMessage(ABM_SETSTATE)`
//! API. Windows then slides the taskbar in whenever the pointer reaches its
//! edge — top, bottom, left or right — on every display.
//!
//! Berth remembers the user's previous setting (in settings.json, so it
//! survives a crash) and puts it back when the option is turned off or Berth
//! quits. Berth never hides, moves or modifies the taskbar window itself.

use windows::core::w;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

use crate::util;

fn data() -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: unsafe { FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default() },
        ..Default::default()
    }
}

/// The taskbar's current state bits, or `None` if no taskbar is running.
fn state() -> Option<u32> {
    let mut d = data();
    if d.hWnd.0.is_null() {
        return None;
    }
    Some(unsafe { SHAppBarMessage(ABM_GETSTATE, &mut d) } as u32)
}

/// Whether Windows' taskbar auto-hide is currently on.
pub fn is_autohide() -> Option<bool> {
    state().map(|s| s & ABS_AUTOHIDE != 0)
}

fn set_bits(bits: u32) -> bool {
    let mut d = data();
    if d.hWnd.0.is_null() {
        return false;
    }
    d.lParam = LPARAM(bits as isize);
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut d) };
    true
}

/// Turns auto-hide on or off, keeping the taskbar's other state bits.
pub fn set_autohide(on: bool) -> bool {
    let Some(cur) = state() else {
        util::log("taskbar not found; auto-hide not changed");
        return false;
    };
    let new = if on {
        cur | ABS_AUTOHIDE
    } else {
        cur & !ABS_AUTOHIDE
    };
    if new == cur {
        return true;
    }
    let ok = set_bits(new);
    util::log(&format!(
        "taskbar auto-hide {}",
        if on { "turned on" } else { "restored to off" }
    ));
    ok
}

// ----- Berth's own hider (taskbar on the left, top or right) ---------------
//
// Windows only honours its auto-hide setting while the taskbar is at the
// bottom. For the other edges Berth hides the taskbar windows itself and
// shows them again when the pointer reaches that edge, the Start menu or
// another shell pop-up is open, or the taskbar has keyboard focus. While
// hidden, Berth also lets apps use the strip the taskbar normally reserves
// (the monitor's work area); the original work area is put back afterwards.

use std::time::{Duration, Instant};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, ShowWindowAsync, SystemParametersInfoW,
    SPIF_SENDCHANGE, SPI_SETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarEdge {
    Left,
    Top,
    Right,
    Bottom,
}

#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub hwnd: HWND,
    pub rect: RECT,
    pub monitor: RECT,
    pub work: RECT,
    pub edge: BarEdge,
}

fn monitor_of(hwnd: HWND) -> Option<(RECT, RECT)> {
    unsafe {
        let m = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(m, &mut mi)
            .as_bool()
            .then_some((mi.rcMonitor, mi.rcWork))
    }
}

fn edge_of(bar: RECT, mon: RECT) -> BarEdge {
    let (w, h) = (bar.right - bar.left, bar.bottom - bar.top);
    if h > w {
        if (bar.left - mon.left).abs() <= (mon.right - bar.right).abs() {
            BarEdge::Left
        } else {
            BarEdge::Right
        }
    } else if (bar.top - mon.top).abs() < (mon.bottom - bar.bottom).abs() {
        BarEdge::Top
    } else {
        BarEdge::Bottom
    }
}

/// All taskbars: the main one and the ones on other displays.
pub fn bars() -> Vec<Bar> {
    let mut out = Vec::new();
    unsafe {
        let mut list: Vec<HWND> = Vec::new();
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), None) {
            list.push(h);
        }
        let mut prev: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, prev, w!("Shell_SecondaryTrayWnd"), None) {
            if h.0.is_null() || list.contains(&h) {
                break;
            }
            list.push(h);
            prev = Some(h);
        }
        for hwnd in list {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                continue;
            }
            if let Some((monitor, work)) = monitor_of(hwnd) {
                out.push(Bar {
                    hwnd,
                    rect,
                    monitor,
                    work,
                    edge: edge_of(rect, monitor),
                });
            }
        }
    }
    out
}

/// The edge of the main taskbar, if one is running.
pub fn main_edge() -> Option<BarEdge> {
    bars().first().map(|b| b.edge)
}

fn set_work_area(r: RECT) {
    let mut r = r;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            SPIF_SENDCHANGE,
        );
    }
}

/// The work area Windows would use with this bar visible.
fn work_with_bar(b: &Bar) -> RECT {
    let mut w = b.monitor;
    match b.edge {
        BarEdge::Left => w.left = b.rect.right.max(w.left),
        BarEdge::Right => w.right = b.rect.left.min(w.right),
        BarEdge::Top => w.top = b.rect.bottom.max(w.top),
        BarEdge::Bottom => w.bottom = b.rect.top.min(w.bottom),
    }
    w
}

fn exe_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = vec![0u16; 512];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
        path.rsplit('\\').next().unwrap_or_default().to_string()
    }
}

/// True while Start, search, the system tray overflow, a jump list or
/// another shell pop-up is in front, so the taskbar must stay visible.
fn shell_popup_in_front(bars: &[Bar]) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        if bars.iter().any(|b| b.hwnd == fg) {
            return true;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls).max(0) as usize;
        let class = String::from_utf16_lossy(&cls[..n]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        let exe = exe_name(pid);
        match exe.as_str() {
            // File Explorer windows also belong to explorer.exe.
            "explorer.exe" => !matches!(
                class.as_str(),
                "CabinetWClass" | "ExploreWClass" | "Progman" | "WorkerW"
            ),
            "startmenuexperiencehost.exe"
            | "shellexperiencehost.exe"
            | "searchhost.exe"
            | "searchapp.exe"
            | "shellhost.exe"
            | "textinputhost.exe" => true,
            _ => false,
        }
    }
}

fn cursor() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some(p)
}

fn inside(r: &RECT, p: POINT, grow: i32) -> bool {
    p.x >= r.left - grow && p.x < r.right + grow && p.y >= r.top - grow && p.y < r.bottom + grow
}

fn at_edge(b: &Bar, p: POINT) -> bool {
    let m = &b.monitor;
    let along_x = p.x >= m.left && p.x < m.right;
    let along_y = p.y >= m.top && p.y < m.bottom;
    match b.edge {
        BarEdge::Left => p.x <= m.left + 1 && along_y,
        BarEdge::Right => p.x >= m.right - 2 && along_y,
        BarEdge::Top => p.y <= m.top + 1 && along_x,
        BarEdge::Bottom => p.y >= m.bottom - 2 && along_x,
    }
}

/// Set while Berth's own hider has taskbars hidden (read by the panic hook).
pub static HIDER_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Berth's own taskbar hider. `tick` is called every ~50 ms while active.
#[derive(Default)]
pub struct Hider {
    pub active: bool,
    /// Original work areas, by the bar's window, to restore later.
    saved: Vec<(HWND, RECT)>,
    shown_by_us: bool,
    leave_since: Option<Instant>,
    last_work_fix: Option<Instant>,
}

impl Hider {
    pub fn enable(&mut self) {
        if !self.active {
            self.active = true;
            HIDER_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
            self.shown_by_us = false;
            self.leave_since = None;
            util::log("Berth's taskbar hider turned on");
            self.tick();
        }
    }

    /// Shows every taskbar again and restores the work areas.
    pub fn disable(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        HIDER_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
        for b in bars() {
            unsafe {
                let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
            }
            let original = self
                .saved
                .iter()
                .find(|(h, _)| *h == b.hwnd)
                .map(|(_, r)| *r);
            set_work_area(original.unwrap_or_else(|| work_with_bar(&b)));
        }
        self.saved.clear();
        util::log("Berth's taskbar hider turned off; taskbar shown");
    }

    pub fn tick(&mut self) {
        if !self.active {
            return;
        }
        let bars = bars();
        let Some(p) = cursor() else { return };
        let popup = shell_popup_in_front(&bars);
        let over = bars
            .iter()
            .any(|b| unsafe { IsWindowVisible(b.hwnd).as_bool() } && inside(&b.rect, p, 4));
        let edge = bars.iter().any(|b| at_edge(b, p));
        let want_visible = popup || over || edge;

        if want_visible {
            self.leave_since = None;
        }
        let hide_now = !want_visible && {
            let since = *self.leave_since.get_or_insert_with(Instant::now);
            since.elapsed() >= Duration::from_millis(600)
        };

        for b in &bars {
            if !self.saved.iter().any(|(h, _)| *h == b.hwnd) {
                // Remember the real work area (with the taskbar) once.
                let w = if b.work == b.monitor {
                    work_with_bar(b)
                } else {
                    b.work
                };
                self.saved.push((b.hwnd, w));
            }
            let visible = unsafe { IsWindowVisible(b.hwnd).as_bool() };
            let mut hidden = !visible;
            if want_visible && !visible {
                unsafe {
                    let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
                }
                hidden = false;
            } else if hide_now && visible {
                unsafe {
                    let _ = ShowWindowAsync(b.hwnd, SW_HIDE);
                }
                hidden = true;
                // Free the strip right after hiding (Explorer recomputes the
                // work area when its window changes).
                self.last_work_fix = None;
            }
            // While hidden, let apps use the taskbar's strip. Explorer may
            // take it back, so check again every half second.
            if hidden && b.work != b.monitor {
                let due = self
                    .last_work_fix
                    .map(|t| t.elapsed() >= Duration::from_millis(500))
                    .unwrap_or(true);
                if due {
                    self.last_work_fix = Some(Instant::now());
                    set_work_area(b.monitor);
                }
            }
        }
        self.shown_by_us = want_visible;
    }
}

/// Last-resort restore (used after a crash or by the panic handler): show
/// every taskbar and give back its strip of the screen.
pub fn emergency_restore() {
    for b in bars() {
        unsafe {
            let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
        }
        set_work_area(work_with_bar(&b));
    }
}
