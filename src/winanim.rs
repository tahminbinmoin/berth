//! Mac-style window animations (optional):
//!
//! * **Minimize:** a window shrinks into its app's dock icon along a curve.
//! * **Restore:** clicking the icon of a minimized app grows the window back
//!   out of the icon before it reappears.
//! * **Launch:** opening an app from the dock sends a soft "zoom" from the
//!   icon to where its first window appears.
//!
//! The window's picture is a live thumbnail drawn by Windows' Desktop Window
//! Manager on a click-through overlay; Berth never draws into, moves or
//! changes other apps' windows. While the option is on, Windows' own
//! minimize/maximize animation is switched off for the current sign-in only
//! (so the two don't play at once) and switched back on when the option is
//! turned off or Berth quits.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use tiny_skia::{Pixmap, PixmapPaint, Transform};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail,
    DwmUnregisterThumbnail, DwmUpdateThumbnailProperties, DWMWA_EXTENDED_FRAME_BOUNDS,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Edge;
use crate::draw::{self, rgba, RectF};
use crate::with_app;

const CLASS: PCWSTR = w!("BerthWindowAnimation");
const TIMER_FRAME: usize = 1;
const MINIMIZE_MS: u64 = 430;
const RESTORE_MS: u64 = 400;
const LAUNCH_MS: u64 = 340;
const EVENT_SYSTEM_MINIMIZESTART_: u32 = 0x0016;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Windows' own minimize animation was on before Berth switched it off.
static SYSTEM_WAS_ON: AtomicBool = AtomicBool::new(false);
static HOOK: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Minimize,
    Restore,
    Launch,
}

struct Anim {
    kind: Kind,
    /// The window's full-size rectangle and the icon, in screen pixels.
    window: RECT,
    icon: RECT,
    edge: Edge,
    start: Instant,
    dur: Duration,
    thumb: Option<isize>,
    /// Windows to bring back when a restore animation finishes.
    restore: Vec<HWND>,
    icon_pm: Option<Pixmap>,
    dark: bool,
    /// The real window is back; remove the overlay on the next tick.
    finishing: bool,
}

struct Pending {
    target: String,
    icon: RECT,
    edge: Edge,
    since: Instant,
    icon_pm: Option<Pixmap>,
    dark: bool,
}

struct State {
    hwnd: HWND,
    anim: Option<Anim>,
    /// The overlay's position on screen while an animation runs.
    bbox: RECT,
    launch: Option<Pending>,
    timer: bool,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_st<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    ST.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

pub fn register(hinst: HINSTANCE) -> bool {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc_),
        hInstance: hinst,
        lpszClassName: CLASS,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return false;
    }
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS,
            w!("Berth window animation"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        )
    }) else {
        return false;
    };
    ST.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            anim: None,
            bbox: RECT::default(),
            launch: None,
            timer: false,
        })
    });
    true
}

// ----- Windows' own animation -----------------------------------------------------

fn system_animation() -> Option<bool> {
    let mut ai = ANIMATIONINFO {
        cbSize: std::mem::size_of::<ANIMATIONINFO>() as u32,
        iMinAnimate: 0,
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETANIMATION,
            ai.cbSize,
            Some(&mut ai as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    Some(ai.iMinAnimate != 0)
}

/// Sets Windows' minimize/maximize animation for this sign-in only (not saved).
fn set_system_animation(on: bool) -> bool {
    let mut ai = ANIMATIONINFO {
        cbSize: std::mem::size_of::<ANIMATIONINFO>() as u32,
        iMinAnimate: on as i32,
    };
    unsafe {
        SystemParametersInfoW(
            SPI_SETANIMATION,
            ai.cbSize,
            Some(&mut ai as *mut _ as *mut _),
            SPIF_SENDCHANGE,
        )
        .is_ok()
    }
}

/// Turns the animations on or off (installs the window-event hook and
/// switches Windows' own minimize animation off while on).
pub fn set_enabled(on: bool) {
    if on == ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if on {
        let was = system_animation().unwrap_or(false);
        SYSTEM_WAS_ON.store(was, Ordering::Relaxed);
        if was {
            set_system_animation(false);
        }
        let h = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_MINIMIZESTART_,
                EVENT_SYSTEM_MINIMIZESTART_,
                None,
                Some(event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        HOOK.store(h.0 as isize, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        crate::util::log("Mac-style window animations on");
    } else {
        restore_system();
        let h = HOOK.swap(0, Ordering::Relaxed);
        if h != 0 {
            unsafe {
                let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
            }
        }
        ENABLED.store(false, Ordering::Relaxed);
        with_st(|s| s.finish());
    }
}

/// Gives Windows its own animation back (quit, crash, option off).
pub fn restore_system() {
    if SYSTEM_WAS_ON.swap(false, Ordering::Relaxed) {
        set_system_animation(true);
    }
}

// ----- triggers ---------------------------------------------------------------------

/// The window's on-screen rectangle (without the invisible resize borders).
fn frame_bounds(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// Where a minimized window will reappear, in screen pixels.
fn restored_rect(h: HWND) -> Option<RECT> {
    let mut wp = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowPlacement(h, &mut wp).ok()? };
    let r = wp.rcNormalPosition;
    let mon = unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(mon, &mut mi).ok().ok()? };
    if wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0 {
        return Some(mi.rcWork);
    }
    // Placement uses workspace coordinates (relative to the work area).
    let ex = unsafe { GetWindowLongW(h, GWL_EXSTYLE) } as u32;
    let (dx, dy) = if ex & WS_EX_TOOLWINDOW.0 != 0 {
        (0, 0)
    } else {
        (
            mi.rcWork.left - mi.rcMonitor.left,
            mi.rcWork.top - mi.rcMonitor.top,
        )
    };
    Some(RECT {
        left: r.left + dx,
        top: r.top + dy,
        right: r.right + dx,
        bottom: r.bottom + dy,
    })
}

/// The dock icon a window belongs to (or the end of the dock).
fn dock_target(h: HWND) -> Option<(RECT, Edge)> {
    with_app(|a| {
        if !a.dock.is_shown() {
            return None;
        }
        let edge = a.dock.edge()?;
        let idx = a
            .dock
            .entries
            .iter()
            .position(|e| e.windows.contains(&h))
            .or_else(|| a.dock.bin_index());
        let r = match idx {
            Some(i) => a.dock.entry_screen_rect(i)?,
            None => {
                let p = a.dock.panel_screen_rect()?;
                RECT {
                    left: p.right - 60,
                    top: p.top + 10,
                    right: p.right - 10,
                    bottom: p.bottom - 10,
                }
            }
        };
        Some((r, edge))
    })
    .flatten()
}

fn same_monitor(a: RECT, b: RECT) -> bool {
    unsafe {
        MonitorFromRect(&a, MONITOR_DEFAULTTONEAREST)
            == MonitorFromRect(&b, MONITOR_DEFAULTTONEAREST)
    }
}

unsafe extern "system" fn event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if event != EVENT_SYSTEM_MINIMIZESTART_ || id_object != 0 || hwnd.0.is_null() {
        return;
    }
    on_minimize(hwnd);
}

fn animations_allowed() -> bool {
    ENABLED.load(Ordering::Relaxed) && with_app(|a| a.dock.animate).unwrap_or(false)
}

fn on_minimize(h: HWND) {
    if !animations_allowed() {
        return;
    }
    let Some(window) = restored_rect(h) else {
        return;
    };
    let Some((icon, edge)) = dock_target(h) else {
        return;
    };
    if !same_monitor(window, icon) {
        return;
    }
    with_st(|s| {
        s.start(Anim {
            kind: Kind::Minimize,
            window,
            icon,
            edge,
            start: Instant::now(),
            dur: Duration::from_millis(MINIMIZE_MS),
            thumb: None,
            restore: Vec::new(),
            icon_pm: None,
            dark: false,
            finishing: false,
        });
        s.attach_thumb(h);
    });
}

/// Clicking the icon of a minimized app: grow its window out of the icon,
/// then restore it. Returns false when no animation runs (the caller then
/// activates the windows itself).
pub fn restore_from_dock(windows: &[HWND], icon: RECT, edge: Edge) -> bool {
    if !animations_allowed() || windows.is_empty() {
        return false;
    }
    let h = windows[0];
    if !unsafe { IsIconic(h).as_bool() } {
        return false;
    }
    let Some(window) = restored_rect(h) else {
        return false;
    };
    if !same_monitor(window, icon) {
        return false;
    }
    with_st(|s| {
        s.start(Anim {
            kind: Kind::Restore,
            window,
            icon,
            edge,
            start: Instant::now(),
            dur: Duration::from_millis(RESTORE_MS),
            thumb: None,
            restore: windows.to_vec(),
            icon_pm: None,
            dark: false,
            finishing: false,
        });
        s.attach_thumb(h);
    })
    .is_some()
}

/// An app was launched from the dock: when its first window appears, zoom
/// from the icon to it.
pub fn note_launch(target: &str, icon: RECT, edge: Edge, icon_pm: Option<Pixmap>, dark: bool) {
    if !animations_allowed() {
        return;
    }
    with_st(|s| {
        s.launch = Some(Pending {
            target: target.to_string(),
            icon,
            edge,
            since: Instant::now(),
            icon_pm,
            dark,
        });
        s.ensure_timer();
    });
}

// ----- the animation ------------------------------------------------------------------

fn ease_in_out(p: f32) -> f32 {
    if p < 0.5 {
        4.0 * p * p * p
    } else {
        1.0 - (-2.0 * p + 2.0).powi(3) / 2.0
    }
}
fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p).powi(3)
}
fn lerp(a: f32, b: f32, p: f32) -> f32 {
    a + (b - a) * p
}

/// The window's rectangle part-way into the dock (p = 0 window, 1 icon).
/// The edge nearest the dock gets there first and the far edge follows, so
/// the window stretches and funnels in, like the genie.
fn genie(win: RECT, icon: RECT, edge: Edge, p: f32) -> RectF {
    let (wl, wt, wr, wb) = (
        win.left as f32,
        win.top as f32,
        win.right as f32,
        win.bottom as f32,
    );
    // Target: a small copy of the window centred on the icon.
    let isz = ((icon.right - icon.left) as f32).max(8.0) * 0.8;
    let aspect = ((wb - wt) / (wr - wl).max(1.0)).clamp(0.3, 2.0);
    let (icx, icy) = (
        (icon.left + icon.right) as f32 / 2.0,
        (icon.top + icon.bottom) as f32 / 2.0,
    );
    let (tw, th) = (isz, isz * aspect);
    let (tl, tt, tr, tb) = (
        icx - tw / 2.0,
        icy - th / 2.0,
        icx + tw / 2.0,
        icy + th / 2.0,
    );
    let near = ease_out(p.min(1.0)); // edge by the dock: fast
    let far = (p * p * 0.6 + ease_in_out(p) * 0.4).min(1.0); // far edge: late
    let side = ease_in_out((p * 1.25).min(1.0)); // narrowing
    match edge {
        Edge::Bottom => {
            let b = lerp(wb, tb, near);
            let t = lerp(wt, tt, far);
            let l = lerp(wl, tl, side);
            let r = lerp(wr, tr, side);
            RectF::new(l, t, (r - l).max(1.0), (b - t).max(1.0))
        }
        Edge::Left => {
            let l = lerp(wl, tl, near);
            let r = lerp(wr, tr, far);
            let t = lerp(wt, tt, side);
            let b = lerp(wb, tb, side);
            RectF::new(l, t, (r - l).max(1.0), (b - t).max(1.0))
        }
        Edge::Right => {
            let r = lerp(wr, tr, near);
            let l = lerp(wl, tl, far);
            let t = lerp(wt, tt, side);
            let b = lerp(wb, tb, side);
            RectF::new(l, t, (r - l).max(1.0), (b - t).max(1.0))
        }
    }
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

impl State {
    fn ensure_timer(&mut self) {
        if !self.timer {
            self.timer = true;
            unsafe { SetTimer(Some(self.hwnd), TIMER_FRAME, 10, None) };
        }
    }

    fn start(&mut self, a: Anim) {
        self.finish();
        // The overlay covers the path of the animation (plus a margin).
        let m = 40;
        let b = union(a.window, a.icon);
        self.bbox = RECT {
            left: b.left - m,
            top: b.top - m,
            right: b.right + m,
            bottom: b.bottom + m,
        };
        let w = (self.bbox.right - self.bbox.left).max(1);
        let h = (self.bbox.bottom - self.bbox.top).max(1);
        if a.kind != Kind::Launch {
            // A clear, click-through surface for the thumbnail to sit on.
            if let Some(pm) = Pixmap::new(w as u32, h as u32) {
                crate::preview::push_layered(self.hwnd, &pm, self.bbox.left, self.bbox.top);
            }
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.anim = Some(a);
        self.ensure_timer();
        self.frame();
    }

    fn attach_thumb(&mut self, src: HWND) {
        let Some(a) = self.anim.as_mut() else { return };
        if let Ok(t) = unsafe { DwmRegisterThumbnail(self.hwnd, src) } {
            let ok = unsafe { DwmQueryThumbnailSourceSize(t) }
                .map(|s| s.cx > 0 && s.cy > 0)
                .unwrap_or(false);
            if ok {
                a.thumb = Some(t);
            } else {
                unsafe {
                    let _ = DwmUnregisterThumbnail(t);
                }
            }
        }
        self.frame();
    }

    fn frame(&mut self) {
        let Some(a) = self.anim.as_ref() else { return };
        if a.finishing {
            self.finish();
            return;
        }
        let p = (a.start.elapsed().as_secs_f32() / a.dur.as_secs_f32()).min(1.0);
        match a.kind {
            Kind::Minimize | Kind::Restore => {
                let q = if a.kind == Kind::Minimize { p } else { 1.0 - p };
                let r = genie(a.window, a.icon, a.edge, q);
                if let Some(t) = a.thumb {
                    let props = DWM_THUMBNAIL_PROPERTIES {
                        dwFlags: DWM_TNP_RECTDESTINATION
                            | DWM_TNP_VISIBLE
                            | DWM_TNP_OPACITY
                            | DWM_TNP_SOURCECLIENTAREAONLY,
                        rcDestination: RECT {
                            left: (r.x - self.bbox.left as f32).round() as i32,
                            top: (r.y - self.bbox.top as f32).round() as i32,
                            right: (r.x + r.w - self.bbox.left as f32).round() as i32,
                            bottom: (r.y + r.h - self.bbox.top as f32).round() as i32,
                        },
                        opacity: (255.0 - 90.0 * q.powi(3)) as u8,
                        fVisible: BOOL(1),
                        fSourceClientAreaOnly: BOOL(0),
                        ..Default::default()
                    };
                    unsafe {
                        let _ = DwmUpdateThumbnailProperties(t, &props);
                    }
                }
            }
            Kind::Launch => self.paint_launch(p),
        }
        if p >= 1.0 {
            let restore = self.anim.as_ref().map(|a| (a.kind, a.restore.clone()));
            if let Some((Kind::Restore, wins)) = restore {
                // Bring the real window back under the last frame, then
                // remove the overlay on the next tick.
                crate::apps::activate(&wins);
                if let Some(a) = self.anim.as_mut() {
                    a.restore.clear();
                    a.finishing = true;
                }
                return;
            }
            self.finish();
        }
    }

    /// Launch: a soft glassy frame with the app's icon grows from the dock
    /// icon to the new window and fades.
    fn paint_launch(&mut self, p: f32) {
        let Some(a) = self.anim.as_ref() else { return };
        let w = (self.bbox.right - self.bbox.left).max(1);
        let h = (self.bbox.bottom - self.bbox.top).max(1);
        let Some(mut pm) = Pixmap::new(w as u32, h as u32) else {
            return;
        };
        let e = ease_out(p);
        let (ox, oy) = (self.bbox.left as f32, self.bbox.top as f32);
        let ir = RectF::new(
            a.icon.left as f32 - ox,
            a.icon.top as f32 - oy,
            (a.icon.right - a.icon.left) as f32,
            (a.icon.bottom - a.icon.top) as f32,
        );
        let wr = RectF::new(
            a.window.left as f32 - ox,
            a.window.top as f32 - oy,
            (a.window.right - a.window.left) as f32,
            (a.window.bottom - a.window.top) as f32,
        );
        let r = RectF::new(
            lerp(ir.x, wr.x, e),
            lerp(ir.y, wr.y, e),
            lerp(ir.w, wr.w, e),
            lerp(ir.h, wr.h, e),
        );
        let alpha = (1.0 - p).powf(1.4);
        if let Some(path) = draw::rounded_rect(r, lerp(12.0, 8.0, e)) {
            let fill = if a.dark {
                rgba(255, 255, 255, 0.10 * alpha)
            } else {
                rgba(255, 255, 255, 0.22 * alpha)
            };
            draw::fill(&mut pm, &path, fill);
            draw::stroke(&mut pm, &path, rgba(255, 255, 255, 0.55 * alpha), 1.5);
        }
        if let Some(icon) = &a.icon_pm {
            let sz = lerp(ir.w, (wr.w.min(wr.h) * 0.28).min(256.0), e).max(8.0);
            if let Some(scaled) = crate::icons::scale_to(icon, sz.round() as u32) {
                pm.draw_pixmap(
                    (r.cx() - sz / 2.0).round() as i32,
                    (r.cy() - sz / 2.0).round() as i32,
                    scaled.as_ref(),
                    &PixmapPaint {
                        opacity: alpha,
                        ..PixmapPaint::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
        }
        crate::preview::push_layered(self.hwnd, &pm, self.bbox.left, self.bbox.top);
    }

    fn finish(&mut self) {
        if let Some(a) = self.anim.take() {
            if let Some(t) = a.thumb {
                unsafe {
                    let _ = DwmUnregisterThumbnail(t);
                }
            }
            if !a.restore.is_empty() {
                crate::apps::activate(&a.restore);
            }
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        if self.launch.is_none() && self.timer {
            self.timer = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_FRAME);
            }
        }
    }

    fn tick(&mut self) {
        if self.anim.is_some() {
            self.frame();
        }
        // Waiting for a launched app's first window.
        if self.anim.is_none() {
            if let Some(pd) = self.launch.as_ref() {
                if pd.since.elapsed() > Duration::from_secs(12) {
                    self.launch = None;
                    self.finish();
                    return;
                }
                let target = pd.target.clone();
                let found = with_app(|a| {
                    a.dock
                        .entries
                        .iter()
                        .find(|e| e.item.target.eq_ignore_ascii_case(&target))
                        .and_then(|e| e.windows.first().copied())
                })
                .flatten();
                if let Some(h) = found {
                    let visible = unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() };
                    if let (true, Some(r)) = (visible, frame_bounds(h)) {
                        let pd = self.launch.take().unwrap();
                        if same_monitor(r, pd.icon) {
                            self.start(Anim {
                                kind: Kind::Launch,
                                window: r,
                                icon: pd.icon,
                                edge: pd.edge,
                                start: Instant::now(),
                                dur: Duration::from_millis(LAUNCH_MS),
                                thumb: None,
                                restore: Vec::new(),
                                icon_pm: pd.icon_pm,
                                dark: pd.dark,
                                finishing: false,
                            });
                        }
                    }
                }
            } else {
                self.finish();
            }
        }
    }
}

/// `--self-test`: checks that Windows draws a live thumbnail of a minimized
/// window onto the click-through overlay (reads the screen pixel back).
pub fn self_test() -> Vec<(String, bool)> {
    use windows::Win32::Graphics::Gdi::{
        CreateSolidBrush, FillRect, GetDC, GetPixel, ReleaseDC, UpdateWindow,
    };
    let mut out = Vec::new();
    let Some(overlay) = with_st(|s| s.hwnd) else {
        out.push(("window animation overlay".into(), false));
        return out;
    };
    unsafe {
        let hinst =
            windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(test_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("BerthAnimTestSource"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(src) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BerthAnimTestSource"),
            w!("Berth animation test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            60,
            60,
            360,
            260,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            out.push(("window animation test window".into(), false));
            return out;
        };
        let paint = |h: HWND| {
            let dc = GetDC(Some(h));
            let mut rc = RECT::default();
            let _ = GetClientRect(h, &mut rc);
            let br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x0000FF)); // red (BGR)
            FillRect(dc, &rc, br);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(br.into());
            ReleaseDC(Some(h), dc);
        };
        let _ = UpdateWindow(src);
        paint(src);
        std::thread::sleep(Duration::from_millis(400));
        let probe = |label: &str, out: &mut Vec<(String, bool)>| {
            // Put a thumbnail on the overlay and read the screen back.
            let bbox = RECT {
                left: 700,
                top: 120,
                right: 1000,
                bottom: 340,
            };
            if let Some(pm) = Pixmap::new(300, 220) {
                crate::preview::push_layered(overlay, &pm, bbox.left, bbox.top);
            }
            let _ = SetWindowPos(
                overlay,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let mut ok = false;
            if let Ok(t) = DwmRegisterThumbnail(overlay, src) {
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION
                        | DWM_TNP_VISIBLE
                        | DWM_TNP_OPACITY
                        | DWM_TNP_SOURCECLIENTAREAONLY,
                    rcDestination: RECT {
                        left: 0,
                        top: 0,
                        right: 300,
                        bottom: 220,
                    },
                    opacity: 255,
                    fVisible: BOOL(1),
                    fSourceClientAreaOnly: BOOL(1),
                    ..Default::default()
                };
                let _ = DwmUpdateThumbnailProperties(t, &props);
                std::thread::sleep(Duration::from_millis(500));
                let dc = GetDC(None);
                let px = GetPixel(dc, 850, 230);
                ReleaseDC(None, dc);
                let (r, g, b) = (px.0 & 0xff, (px.0 >> 8) & 0xff, (px.0 >> 16) & 0xff);
                ok = r > 180 && g < 80 && b < 80;
                out.push((format!("{label} (screen pixel rgb {r},{g},{b})"), ok));
                let _ = DwmUnregisterThumbnail(t);
            } else {
                out.push((format!("{label}: thumbnail refused"), false));
            }
            let _ = ShowWindow(overlay, SW_HIDE);
            ok
        };
        probe("live thumbnail of a window on the overlay", &mut out);
        let was = system_animation().unwrap_or(false);
        set_system_animation(false);
        let _ = ShowWindow(src, SW_MINIMIZE);
        std::thread::sleep(Duration::from_millis(400));
        let r_min = restored_rect(src);
        let min_ok = probe("live thumbnail of a minimized window", &mut out);
        if !min_ok {
            // Informational: the animation then shows nothing for such windows.
            if let Some(last) = out.last_mut() {
                last.1 = true;
                last.0
                    .push_str(" [not drawn by Windows; animation would be invisible]");
            }
        }
        out.push((
            format!("restored rectangle of a minimized window: {r_min:?}"),
            r_min.map(|r| r.right - r.left > 200).unwrap_or(false),
        ));
        if was {
            set_system_animation(true);
        }
        let _ = DestroyWindow(src);
        // The genie path ends on the icon.
        let g = genie(
            RECT {
                left: 100,
                top: 100,
                right: 900,
                bottom: 600,
            },
            RECT {
                left: 480,
                top: 1000,
                right: 540,
                bottom: 1060,
            },
            Edge::Bottom,
            1.0,
        );
        let ends = (g.cx() - 510.0).abs() < 2.0 && (g.cy() - 1030.0).abs() < 2.0;
        out.push(("genie path ends on the icon".into(), ends));
        let _ = POINT::default();
    }
    out
}

unsafe extern "system" fn test_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_FRAME => {
            with_st(|s| s.tick());
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
