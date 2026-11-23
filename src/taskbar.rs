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
// another shell pop-up is open, or the taskbar has keyboard focus. The
// strip the taskbar reserves stays reserved (Berth doesn't touch the work
// area).

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
///
/// It only shows and hides the taskbar windows. It never changes the
/// screen's work area: asking Windows to give the taskbar's strip to apps
/// made Explorer take it back again and again, and every round resized all
/// maximized windows (the "jumping" in 0.1.4).
#[derive(Default)]
pub struct Hider {
    pub active: bool,
    leave_since: Option<Instant>,
    /// When Berth last hid the taskbar.
    hidden_at: Option<Instant>,
    /// Times Explorer re-showed the taskbar right after Berth hid it.
    fights: Vec<Instant>,
    /// Berth backs off (leaves the taskbar visible) until this time.
    paused_until: Option<Instant>,
}

impl Hider {
    pub fn enable(&mut self) {
        if !self.active {
            self.active = true;
            HIDER_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
            self.leave_since = None;
            self.fights.clear();
            self.paused_until = None;
            repair_work_areas();
            util::log("Berth's taskbar hider turned on");
            self.tick();
        }
    }

    /// Shows every taskbar again.
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
        }
        repair_work_areas();
        util::log("Berth's taskbar hider turned off; taskbar shown");
    }

    pub fn tick(&mut self) {
        if !self.active {
            return;
        }
        let bars = bars();
        let Some(p) = cursor() else { return };
        let now = Instant::now();
        if let Some(t) = self.paused_until {
            if now < t {
                return;
            }
            self.paused_until = None;
        }
        let popup = shell_popup_in_front(&bars);
        let over = bars
            .iter()
            .any(|b| unsafe { IsWindowVisible(b.hwnd).as_bool() } && inside(&b.rect, p, 4));
        let edge = bars.iter().any(|b| at_edge(b, p));
        let want_visible = popup || over || edge;

        if want_visible {
            self.leave_since = None;
        }
        let any_visible = bars
            .iter()
            .any(|b| unsafe { IsWindowVisible(b.hwnd).as_bool() });

        // Explorer re-showed the taskbar right after Berth hid it: count it,
        // and if it keeps happening, stop fighting for a while so the screen
        // stays calm.
        if !want_visible && any_visible {
            if let Some(h) = self.hidden_at.take() {
                if h.elapsed() < Duration::from_millis(1500) {
                    self.fights
                        .retain(|t| t.elapsed() < Duration::from_secs(15));
                    self.fights.push(now);
                    if self.fights.len() >= 4 {
                        self.fights.clear();
                        self.paused_until = Some(now + Duration::from_secs(30));
                        util::log(
                            "Explorer keeps showing the taskbar; Berth pauses hiding it for 30 s",
                        );
                        return;
                    }
                }
            }
        }

        let hide_now = !want_visible && {
            let since = *self.leave_since.get_or_insert(now);
            since.elapsed() >= Duration::from_millis(600)
        };

        for b in &bars {
            let visible = unsafe { IsWindowVisible(b.hwnd).as_bool() };
            if want_visible && !visible {
                unsafe {
                    let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
                }
            } else if hide_now && visible {
                unsafe {
                    let _ = ShowWindowAsync(b.hwnd, SW_HIDE);
                }
                self.hidden_at = Some(now);
            }
        }
    }
}

/// Undoes a work area left at full-screen size by Berth 0.1.4 (only if the
/// taskbar's strip is currently not reserved).
fn repair_work_areas() {
    let native_autohide = is_autohide() == Some(true);
    for b in bars() {
        // With Windows' own auto-hide at the bottom, a full-size work area is correct.
        if b.edge == BarEdge::Bottom && native_autohide {
            continue;
        }
        if b.work == b.monitor {
            let _ = set_work_area(work_with_bar(&b));
        }
    }
}

/// Last-resort restore (used after a crash or by the panic handler): show
/// every taskbar and give back its strip of the screen.
pub fn emergency_restore() {
    for b in bars() {
        unsafe {
            let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
        }
    }
    repair_work_areas();
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

/// Developer experiment (CI only): how does Windows treat the work area
/// when the taskbar is hidden and the work area is changed with or
/// without a broadcast? Returns a log of observations.
pub fn experiment() -> Vec<String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, ShowWindow, SW_MAXIMIZE, WINDOW_EX_STYLE,
        WS_OVERLAPPEDWINDOW,
    };
    let mut out = Vec::new();
    let Some(b) = bars().first().copied() else {
        out.push("no taskbar".into());
        return out;
    };
    let wait = |ms| std::thread::sleep(Duration::from_millis(ms));
    let fmt = |r: RECT| format!("{},{}-{},{}", r.left, r.top, r.right, r.bottom);
    let work = || bars().first().map(|b| b.work).unwrap_or_default();
    // A maximized test window, to see whether apps get resized.
    let test = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("berth-experiment"),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            300,
            200,
            None,
            None,
            None,
            None,
        )
        .ok()
    };
    let win_rect = || {
        let mut r = RECT::default();
        if let Some(h) = test {
            unsafe {
                let _ = GetWindowRect(h, &mut r);
            }
        }
        fmt(r)
    };
    if let Some(h) = test {
        unsafe {
            let _ = ShowWindow(h, SW_MAXIMIZE);
        }
    }
    wait(500);
    out.push(format!(
        "monitor {} work {} bar {:?} {} max-window {}",
        fmt(b.monitor),
        fmt(work()),
        b.edge,
        fmt(b.rect),
        win_rect()
    ));
    unsafe {
        let _ = ShowWindowAsync(b.hwnd, SW_HIDE);
    }
    wait(600);
    out.push(format!(
        "after hide: work {} max-window {}",
        fmt(work()),
        win_rect()
    ));
    // A: quiet set (no broadcast)
    let mut r = b.monitor;
    let ra = unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            Default::default(),
        )
    };
    for ms in [100u64, 400, 1000, 2000] {
        wait(ms);
        out.push(format!(
            "A quiet set ({}) +{ms}ms: work {} max-window {}",
            ra.is_ok(),
            fmt(work()),
            win_rect()
        ));
    }
    // Reveal and hide again.
    unsafe {
        let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
    }
    wait(600);
    out.push(format!(
        "A after reveal: work {} max-window {}",
        fmt(work()),
        win_rect()
    ));
    unsafe {
        let _ = ShowWindowAsync(b.hwnd, SW_HIDE);
    }
    wait(600);
    out.push(format!(
        "A after re-hide: work {} max-window {}",
        fmt(work()),
        win_rect()
    ));
    // B: broadcast set
    let mut r = b.monitor;
    let rb = unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            SPIF_SENDCHANGE,
        )
    };
    for ms in [100u64, 400, 1000, 2000] {
        wait(ms);
        out.push(format!(
            "B broadcast set ({}) +{ms}ms: work {} max-window {}",
            rb.is_ok(),
            fmt(work()),
            win_rect()
        ));
    }
    // Restore Explorer's view first.
    unsafe {
        let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
    }
    let _ = set_work_area(work_with_bar(&b));
    wait(800);
    out.push(format!(
        "C start: work {} max-window {}",
        fmt(work()),
        win_rect()
    ));
    // C: keep the taskbar shown but clip it to an empty region.
    unsafe {
        use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
        let r = SetWindowRgn(b.hwnd, Some(CreateRectRgn(0, 0, 0, 0)), true);
        out.push(format!("C empty region set: {r}"));
    }
    wait(600);
    out.push(format!(
        "C after region-hide: work {} visible {}",
        fmt(work()),
        unsafe { IsWindowVisible(b.hwnd).as_bool() }
    ));
    let mut r = b.monitor;
    let rc = unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            Default::default(),
        )
    };
    let moved = resize_maximized(work_with_bar(&b), b.monitor);
    for ms in [100u64, 400, 1000, 2000] {
        wait(ms);
        out.push(format!(
            "C quiet set ({}) moved {moved} +{ms}ms: work {} max-window {}",
            rc.is_ok(),
            fmt(work()),
            win_rect()
        ));
    }
    unsafe {
        use windows::Win32::Graphics::Gdi::SetWindowRgn;
        let _ = SetWindowRgn(b.hwnd, None, true);
    }
    wait(800);
    out.push(format!(
        "C after region-reveal: work {} max-window {}",
        fmt(work()),
        win_rect()
    ));
    unsafe {
        use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
        let _ = SetWindowRgn(b.hwnd, Some(CreateRectRgn(0, 0, 0, 0)), true);
    }
    wait(800);
    out.push(format!(
        "C after region-rehide: work {} max-window {}",
        fmt(work()),
        win_rect()
    ));
    unsafe {
        use windows::Win32::Graphics::Gdi::SetWindowRgn;
        let _ = SetWindowRgn(b.hwnd, None, true);
    }
    // Restore.
    unsafe {
        let _ = ShowWindowAsync(b.hwnd, SW_SHOWNOACTIVATE);
        if let Some(h) = test {
            let _ = DestroyWindow(h);
        }
    }
    wait(500);
    let _ = set_work_area(work_with_bar(&b));
    wait(500);
    out.push(format!("restored: work {}", fmt(work())));
    out
}
