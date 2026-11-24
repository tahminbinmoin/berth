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
// another shell pop-up is open, or the taskbar has keyboard focus. See
// `Hider` for how this avoids fighting Explorer.

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
    SPIF_SENDCHANGE, SPI_SETWORKAREA, SW_SHOWNOACTIVATE,
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

fn set_work_area(r: RECT) -> Result<(), String> {
    let mut r = r;
    unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            SPIF_SENDCHANGE,
        )
        .map_err(|e| e.message().to_string())
    }
}

/// The whole monitor minus the strip Berth's menu bar reserves there.
fn full_area(monitor: RECT) -> RECT {
    let mut w = monitor;
    if let Some(r) = crate::menubar::reserved() {
        if r.left >= monitor.left
            && r.right <= monitor.right
            && r.top >= monitor.top
            && r.top < monitor.bottom
        {
            w.top = w.top.max(r.bottom);
        }
    }
    w
}

/// The work area Windows would use with this bar visible.
fn work_with_bar(b: &Bar) -> RECT {
    let mut w = full_area(b.monitor);
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

fn set_region(hwnd: HWND, b: &Bar, t: f32) {
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
    unsafe {
        if t >= 0.999 {
            let _ = SetWindowRgn(hwnd, None, true);
            return;
        }
        let (w, h) = (b.rect.right - b.rect.left, b.rect.bottom - b.rect.top);
        let (dw, dh) = ((w as f32 * t).round() as i32, (h as f32 * t).round() as i32);
        let rgn = match b.edge {
            BarEdge::Left => CreateRectRgn(0, 0, dw, h),
            BarEdge::Right => CreateRectRgn(w - dw, 0, w, h),
            BarEdge::Top => CreateRectRgn(0, 0, w, dh),
            BarEdge::Bottom => CreateRectRgn(0, h - dh, w, h),
        };
        let _ = SetWindowRgn(hwnd, Some(rgn), true);
    }
}

/// True if the window currently has a window region (Berth's clip).
fn has_region(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, GetWindowRgn, HGDIOBJ};
    unsafe {
        let tmp = CreateRectRgn(0, 0, 0, 0);
        let kind = GetWindowRgn(hwnd, tmp);
        let _ = DeleteObject(HGDIOBJ(tmp.0));
        kind.0 != 0 // 0 = ERROR (no region)
    }
}

fn set_work_area_quiet(r: RECT) -> bool {
    let mut r = r;
    unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            Default::default(),
        )
        .is_ok()
    }
}

struct BarState {
    hwnd: HWND,
    /// 0 = hidden, 1 = fully shown.
    shown: f32,
    target: f32,
    applied: f32,
}

/// Berth's own taskbar hider, used when the taskbar is on the left, top or
/// right (Windows only allows its own auto-hide at the bottom).
///
/// Hiding the taskbar window makes Explorer reserve its strip again and
/// resize every window, so Berth never hides it. The taskbar stays "shown"
/// for Explorer but is clipped to an empty window region (invisible and
/// click-through); revealing it grows the region out from the screen edge.
/// The strip is given to apps by updating the work area *without* a
/// system-wide broadcast (a broadcast makes Explorer take it straight back),
/// and maximized windows are resized to fill it.
#[derive(Default)]
pub struct Hider {
    pub active: bool,
    bars: Vec<BarState>,
    leave_since: Option<Instant>,
    last_work_fix: Option<Instant>,
    pub animating: bool,
}

impl Hider {
    pub fn enable(&mut self) {
        if !self.active {
            self.active = true;
            HIDER_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
            self.leave_since = Instant::now().checked_sub(Duration::from_secs(5));
            self.bars.clear();
            util::log("Berth's taskbar hider turned on");
            self.tick(false);
        }
    }

    /// Shows every taskbar again and gives its strip back.
    pub fn disable(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        HIDER_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
        self.bars.clear();
        restore_bars();
        util::log("Berth's taskbar hider turned off; taskbar shown");
    }

    /// Called every 15–50 ms. `animate` enables the reveal animation.
    pub fn tick(&mut self, animate: bool) {
        if !self.active {
            return;
        }
        let bars = bars();
        let Some(p) = cursor() else { return };
        let now = Instant::now();

        // Forget taskbars that no longer exist (Explorer restarted).
        self.bars.retain(|s| bars.iter().any(|b| b.hwnd == s.hwnd));

        let popup = shell_popup_in_front(&bars);
        let over = bars.iter().any(|b| {
            let shown = self
                .bars
                .iter()
                .find(|s| s.hwnd == b.hwnd)
                .map(|s| s.shown > 0.5)
                .unwrap_or(true);
            shown && inside(&b.rect, p, 4)
        });
        let edge = bars.iter().any(|b| at_edge(b, p));
        let want_visible = popup || over || edge;
        if want_visible {
            self.leave_since = None;
        }
        let hide = !want_visible && {
            let since = *self.leave_since.get_or_insert(now);
            since.elapsed() >= Duration::from_millis(500)
        };

        self.animating = false;
        for b in &bars {
            if !self.bars.iter().any(|s| s.hwnd == b.hwnd) {
                // A taskbar Berth hasn't handled yet (or a new Explorer):
                // undo any older Berth version's SW_HIDE.
                unsafe {
                    let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
                }
                self.bars.push(BarState {
                    hwnd: b.hwnd,
                    shown: 1.0,
                    target: 1.0,
                    applied: -1.0,
                });
            }
            let Some(st) = self.bars.iter_mut().find(|s| s.hwnd == b.hwnd) else {
                continue;
            };
            if want_visible {
                st.target = 1.0;
            } else if hide {
                st.target = 0.0;
            }
            if st.shown != st.target {
                if animate {
                    // Reveal over ~150 ms, hide over ~100 ms (at ~60 fps).
                    st.shown = if st.target > st.shown {
                        (st.shown + 0.12).min(st.target)
                    } else {
                        (st.shown - 0.18).max(st.target)
                    };
                } else {
                    st.shown = st.target;
                }
            }
            if st.shown != st.target {
                self.animating = true;
            }
            // Explorer can drop the region (e.g. after a DPI change).
            let lost = st.applied < 0.999 && st.applied >= 0.0 && !has_region(b.hwnd);
            if (st.shown - st.applied).abs() > 0.001 || lost {
                set_region(b.hwnd, b, ease(st.shown));
                st.applied = st.shown;
            }
            // Keep the strip for apps (without a broadcast). Explorer may
            // reclaim it after display or settings changes.
            let full = full_area(b.monitor);
            if b.work != full {
                let due = self
                    .last_work_fix
                    .map(|t| t.elapsed() >= Duration::from_secs(1))
                    .unwrap_or(true);
                if due {
                    self.last_work_fix = Some(now);
                    if set_work_area_quiet(full) {
                        resize_maximized(b.work, full);
                    }
                }
            }
        }
    }
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Shows every taskbar (removes Berth's clip, undoes an older SW_HIDE) and
/// gives its strip back, unless Windows' own auto-hide is in use.
fn restore_bars() {
    let native_autohide = is_autohide() == Some(true);
    for b in bars() {
        unsafe {
            use windows::Win32::Graphics::Gdi::SetWindowRgn;
            let _ = SetWindowRgn(b.hwnd, None, true);
            let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
        }
        if b.edge == BarEdge::Bottom && native_autohide {
            continue;
        }
        let normal = work_with_bar(&b);
        if b.work != normal {
            if set_work_area_quiet(normal) {
                resize_maximized(b.work, normal);
            }
            // Let Explorer recompute its own value too.
            let _ = set_work_area(normal);
        }
    }
}

/// Last-resort restore (after an unexpected exit, or from the panic
/// handler): show every taskbar and give it its strip back.
pub fn emergency_restore() {
    crate::menubar::shutdown();
    crate::winanim::restore_system();
    restore_bars();
}

/// Resizes maximized windows on a monitor from the old to the new work
/// area (keeping their frame overhang). Returns how many were moved.
pub fn resize_maximized(old_work: RECT, new_work: RECT) -> usize {
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, IsZoomed, SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
        SWP_NOZORDER,
    };
    unsafe extern "system" fn collect(
        h: HWND,
        l: windows::Win32::Foundation::LPARAM,
    ) -> windows::core::BOOL {
        (*(l.0 as *mut Vec<HWND>)).push(h);
        windows::core::BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect),
            windows::Win32::Foundation::LPARAM(&mut all as *mut _ as isize),
        );
    }
    let mut n = 0;
    for h in all {
        unsafe {
            if !IsWindowVisible(h).as_bool() || !IsZoomed(h).as_bool() {
                continue;
            }
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_err() {
                continue;
            }
            // Only windows maximized on this monitor's old work area.
            let cx = (r.left + r.right) / 2;
            let cy = (r.top + r.bottom) / 2;
            if cx < old_work.left
                || cx >= old_work.right
                || cy < old_work.top
                || cy >= old_work.bottom
            {
                continue;
            }
            let (il, it) = (old_work.left - r.left, old_work.top - r.top);
            let (ir, ib) = (r.right - old_work.right, r.bottom - old_work.bottom);
            if il.abs() > 16 || it.abs() > 16 || ir.abs() > 16 || ib.abs() > 16 {
                continue; // not sized to the work area; leave it alone
            }
            let x = new_work.left - il;
            let y = new_work.top - it;
            let w = (new_work.right + ir) - x;
            let hgt = (new_work.bottom + ib) - y;
            let _ = SetWindowPos(
                h,
                None,
                x,
                y,
                w,
                hgt,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            );
            n += 1;
        }
    }
    n
}

/// Whether Berth's clip region is on a taskbar window (for the self-test).
pub fn is_clipped(hwnd: HWND) -> bool {
    has_region(hwnd)
}
