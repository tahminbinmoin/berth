//! The optional Mac-style menu bar along the top of the dock's display.
//!
//! It is a thin, top-most strip registered with Windows as an application
//! desktop toolbar ("AppBar", the same mechanism the taskbar uses), so
//! Windows keeps maximized windows below it. From left to right it shows:
//! a Berth menu (settings, Task Manager, sleep, restart, shut down, lock,
//! sign out), the name of the app in front with an app menu (hide, hide
//! others, show all, quit), a Window menu (minimize, zoom, close), and on the
//! right search, the input language, a Quick Settings group (Wi-Fi, volume,
//! battery) and the date and time (which opens the notification center).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use tiny_skia::{Color, Pixmap, PixmapPaint, Transform};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetKeyboardLayoutList, SendInput, TrackMouseEvent, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_A,
    VK_LWIN, VK_N, VK_S, VK_SPACE,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABM_WINDOWPOSCHANGED,
    ABN_FULLSCREENAPP, ABN_POSCHANGED, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::draw::{self, rgba, RectF};
use crate::system::{self, Monitor};
use crate::{apps, text, util};

const CLASS: PCWSTR = w!("BerthMenuBar");
const TIMER_TICK: usize = 1;
const TICK_MS: u32 = 250;
const WM_APPBAR: u32 = WM_APP + 40;
const WM_MOUSELEAVE_: u32 = 0x02A3;
const TEXT_FACE: &str = "Segoe UI Variable Text";
const ICON_FACE: &str = "Segoe Fluent Icons";

/// The window registered as an AppBar (for removal from anywhere, even
/// the panic handler).
static APPBAR: AtomicIsize = AtomicIsize::new(0);
/// The strip the menu bar reserves, for Berth's taskbar hider.
static RESERVED: Mutex<Option<RECT>> = Mutex::new(None);

/// The screen strip the menu bar occupies, if it is on.
pub fn reserved() -> Option<RECT> {
    RESERVED.lock().ok().and_then(|r| *r)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    System,
    App,
    Window,
    Search,
    Layout,
    Quick,
    Clock,
}

#[derive(Clone, Copy)]
struct Slot {
    item: Item,
    x0: f32,
    x1: f32,
}

struct Bar {
    hwnd: HWND,
    enabled: bool,
    shown: bool,
    rect: RECT,
    monitor: Option<Monitor>,
    scale: f32,
    dark: bool,
    /// The app in front (never Berth itself) and its main window.
    app_hwnd: HWND,
    app_target: String,
    app_name: String,
    app_pid: u32,
    desktop: bool,
    names: HashMap<String, String>,
    clock: String,
    layout: Option<String>,
    battery: Option<(u8, bool)>,
    fullscreen: bool,
    hot: Option<Item>,
    open: Option<Item>,
    tracking: bool,
    slots: Vec<Slot>,
    ticks: u32,
}

thread_local! {
    static MB: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

fn with_mb<R>(f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    MB.with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.as_mut().map(f)))
}

pub fn register(hinst: HINSTANCE) -> bool {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc_),
        hInstance: hinst,
        lpszClassName: CLASS,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return false;
    }
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS,
            w!("Berth menu bar"),
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
    MB.with(|m| {
        *m.borrow_mut() = Some(Bar {
            hwnd,
            enabled: false,
            shown: false,
            rect: RECT::default(),
            monitor: None,
            scale: 1.0,
            dark: false,
            app_hwnd: HWND::default(),
            app_target: String::new(),
            app_name: "Desktop".into(),
            app_pid: 0,
            desktop: true,
            names: HashMap::new(),
            clock: String::new(),
            layout: None,
            battery: None,
            fullscreen: false,
            hot: None,
            open: None,
            tracking: false,
            slots: Vec::new(),
            ticks: 0,
        })
    });
    true
}

/// Turns the menu bar on or off and places it on `monitor`.
pub fn sync(enabled: bool, monitor: Option<Monitor>, dark: bool) {
    with_mb(|b| {
        b.dark = dark;
        let monitor = monitor.filter(|_| enabled);
        match monitor {
            Some(m) => {
                let moved = b
                    .monitor
                    .as_ref()
                    .map(|o| o.rect != m.rect || o.dpi != m.dpi)
                    != Some(false);
                b.scale = m.scale();
                b.monitor = Some(m);
                if !b.enabled || moved {
                    b.enabled = true;
                    b.place();
                    unsafe { SetTimer(Some(b.hwnd), TIMER_TICK, TICK_MS, None) };
                }
                b.refresh(true);
            }
            None => {
                if b.enabled {
                    b.enabled = false;
                    b.remove();
                    unsafe {
                        let _ = KillTimer(Some(b.hwnd), TIMER_TICK);
                    }
                }
            }
        }
    });
}

/// Gives the reserved strip back (quit, sign-out, or after a crash).
pub fn shutdown() {
    let h = APPBAR.swap(0, Ordering::Relaxed);
    if h != 0 {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: HWND(h as *mut _),
            ..Default::default()
        };
        unsafe {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            let _ = ShowWindow(HWND(h as *mut _), SW_HIDE);
        }
        if let Ok(mut r) = RESERVED.lock() {
            *r = None;
        }
    }
}

impl Bar {
    fn height(&self) -> i32 {
        (28.0 * self.scale).round() as i32
    }

    fn appbar_data(&self) -> APPBARDATA {
        APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: WM_APPBAR,
            uEdge: ABE_TOP,
            ..Default::default()
        }
    }

    /// Registers the AppBar (once) and asks Windows for the top strip.
    fn place(&mut self) {
        let Some(m) = self.monitor.clone() else {
            return;
        };
        let h = self.height();
        let mut abd = self.appbar_data();
        unsafe {
            if APPBAR.load(Ordering::Relaxed) == 0 {
                if SHAppBarMessage(ABM_NEW, &mut abd) == 0 {
                    util::log("menu bar: Windows refused to register it");
                    return;
                }
                APPBAR.store(self.hwnd.0 as isize, Ordering::Relaxed);
            }
            abd.rc = RECT {
                left: m.rect.left,
                top: m.rect.top,
                right: m.rect.right,
                bottom: m.rect.top + h,
            };
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.bottom = abd.rc.top + h;
            SHAppBarMessage(ABM_SETPOS, &mut abd);
        }
        // Windows may shorten the strip beside a side taskbar; the bar itself
        // still spans the whole display, like on a Mac.
        self.rect = RECT {
            left: m.rect.left,
            right: m.rect.right,
            ..abd.rc
        };
        if let Ok(mut r) = RESERVED.lock() {
            *r = Some(self.rect);
        }
    }

    fn remove(&mut self) {
        shutdown();
        self.shown = false;
    }

    /// Re-reads what the bar shows; repaints when something changed.
    fn refresh(&mut self, force: bool) {
        if !self.enabled {
            return;
        }
        self.ticks = self.ticks.wrapping_add(1);
        let mut changed = force;

        // The app in front.
        let fg = unsafe { GetForegroundWindow() };
        if !fg.0.is_null() && fg != self.app_hwnd {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
            let class = apps::window_class(fg);
            if pid == std::process::id()
                || matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
                || class.starts_with("Xaml_WindowedPopupClass")
            {
                // Berth's own windows, the taskbar and shell pop-ups don't
                // change the app shown.
            } else if matches!(class.as_str(), "Progman" | "WorkerW") {
                self.app_hwnd = fg;
                self.desktop = true;
                self.app_pid = 0;
                self.app_target.clear();
                self.app_name = "Desktop".into();
                changed = true;
            } else if let Some(app) = apps::window_app(fg) {
                self.app_hwnd = fg;
                self.desktop = false;
                self.app_pid = app.pid;
                let key = app.target.to_ascii_lowercase();
                let name = self
                    .names
                    .entry(key)
                    .or_insert_with(|| apps::friendly_name(&app.target))
                    .clone();
                self.app_target = app.target;
                if name != self.app_name {
                    self.app_name = name;
                }
                changed = true;
            }
        }

        // Clock (minute resolution), input language, battery.
        let clock = clock_text();
        if clock != self.clock {
            self.clock = clock;
            changed = true;
        }
        let layout = input_language(self.app_hwnd);
        if layout != self.layout {
            self.layout = layout;
            changed = true;
        }
        if force || self.ticks % 40 == 0 {
            let bat = battery();
            if bat != self.battery {
                self.battery = bat;
                changed = true;
            }
        }

        // Step aside for full-screen apps.
        if self.ticks % 4 == 0 || force {
            let fs = self
                .monitor
                .as_ref()
                .map(|m| system::fullscreen_app_on(m.handle, &[self.hwnd]))
                .unwrap_or(false);
            if fs != self.fullscreen {
                self.fullscreen = fs;
                changed = true;
            }
        }

        if changed {
            self.paint();
        }
    }

    fn paint(&mut self) {
        let show = self.enabled && !self.fullscreen;
        if !show {
            if self.shown {
                self.shown = false;
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            return;
        }
        let w = (self.rect.right - self.rect.left).max(1);
        let h = (self.rect.bottom - self.rect.top).max(1);
        let Some(pm) = self.render(w, h) else { return };
        crate::preview::push_layered(self.hwnd, &pm, self.rect.left, self.rect.top);
        if !self.shown {
            self.shown = true;
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
        }
    }

    /// Draws the bar into a pixmap and records where each item is.
    fn render(&mut self, w: i32, h: i32) -> Option<Pixmap> {
        let mut pm = Pixmap::new(w as u32, h as u32)?;
        let s = self.scale;
        let dark = self.dark;
        let hf = h as f32;
        let bg = if dark {
            rgba(24, 24, 28, 0.86)
        } else {
            rgba(246, 246, 249, 0.86)
        };
        pm.fill(bg);
        if let Some(p) = draw::rounded_rect(RectF::new(0.0, hf - 1.0, w as f32, 1.0), 0.0) {
            draw::fill(
                &mut pm,
                &p,
                if dark {
                    rgba(255, 255, 255, 0.08)
                } else {
                    rgba(0, 0, 0, 0.08)
                },
            );
        }
        let fg = if dark { (240, 240, 244) } else { (24, 24, 28) };
        let fgc = rgba(fg.0, fg.1, fg.2, 1.0);
        let text_px = (13.0 * s).round() as i32;
        let icon_px = (14.0 * s).round() as i32;
        let pad = (10.0 * s).round();
        let item_pad = (9.0 * s).round();
        let mut slots: Vec<Slot> = Vec::new();

        // Left side.
        let mut x = (6.0 * s).round();
        let logo_w = (34.0 * s).round();
        slots.push(Slot {
            item: Item::System,
            x0: x,
            x1: x + logo_w,
        });
        x += logo_w;
        let app = text::render_with(
            &self.app_name,
            TEXT_FACE,
            600,
            text_px,
            (360.0 * s) as i32,
            false,
            fg,
        );
        let aw = app.as_ref().map(|p| p.width() as f32).unwrap_or(0.0) + 2.0 * item_pad;
        slots.push(Slot {
            item: Item::App,
            x0: x,
            x1: x + aw,
        });
        x += aw;
        let win = text::render_with("Window", TEXT_FACE, 400, text_px, 200, false, fg);
        let ww = win.as_ref().map(|p| p.width() as f32).unwrap_or(0.0) + 2.0 * item_pad;
        slots.push(Slot {
            item: Item::Window,
            x0: x,
            x1: x + ww,
        });

        // Right side, from the right edge.
        let mut rx = w as f32 - pad;
        let clock = text::render_with(&self.clock, TEXT_FACE, 400, text_px, 300, false, fg);
        let cw = clock.as_ref().map(|p| p.width() as f32).unwrap_or(0.0) + 2.0 * item_pad;
        slots.push(Slot {
            item: Item::Clock,
            x0: rx - cw,
            x1: rx,
        });
        rx -= cw;
        let wifi = text::render_with("\u{E701}", ICON_FACE, 400, icon_px, 100, false, fg);
        let vol = text::render_with("\u{E767}", ICON_FACE, 400, icon_px, 100, false, fg);
        let bat_w = if self.battery.is_some() {
            26.0 * s
        } else {
            0.0
        };
        let gap = (10.0 * s).round();
        let qw = wifi.as_ref().map(|p| p.width() as f32).unwrap_or(0.0)
            + vol.as_ref().map(|p| p.width() as f32).unwrap_or(0.0)
            + bat_w
            + if self.battery.is_some() {
                2.0 * gap
            } else {
                gap
            }
            + 2.0 * item_pad;
        slots.push(Slot {
            item: Item::Quick,
            x0: rx - qw,
            x1: rx,
        });
        rx -= qw;
        let lang = self.layout.as_ref().and_then(|l| {
            text::render_with(l, TEXT_FACE, 600, (11.0 * s).round() as i32, 100, false, fg)
        });
        if let Some(l) = &lang {
            let lw = l.width() as f32 + 2.0 * item_pad;
            slots.push(Slot {
                item: Item::Layout,
                x0: rx - lw,
                x1: rx,
            });
            rx -= lw;
        }
        let search = text::render_with("\u{E721}", ICON_FACE, 400, icon_px, 100, false, fg);
        let sw = search.as_ref().map(|p| p.width() as f32).unwrap_or(0.0) + 2.0 * item_pad;
        slots.push(Slot {
            item: Item::Search,
            x0: rx - sw,
            x1: rx,
        });

        // Highlights.
        let lit = self.open.or(self.hot);
        for sl in &slots {
            if Some(sl.item) == lit {
                let r = RectF::new(sl.x0, 3.0 * s, sl.x1 - sl.x0, hf - 6.0 * s);
                if let Some(p) = draw::rounded_rect(r, 5.0 * s) {
                    draw::fill(
                        &mut pm,
                        &p,
                        if dark {
                            rgba(255, 255, 255, 0.16)
                        } else {
                            rgba(0, 0, 0, 0.09)
                        },
                    );
                }
            }
        }

        let blit = |pm: &mut Pixmap, p: &Option<Pixmap>, x: f32| {
            if let Some(p) = p {
                let y = ((hf - p.height() as f32) / 2.0).round();
                pm.draw_pixmap(
                    x.round() as i32,
                    y as i32,
                    p.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
        };
        for sl in slots.clone() {
            match sl.item {
                Item::System => draw_logo(&mut pm, sl, hf, s, fgc),
                Item::App => blit(&mut pm, &app, sl.x0 + item_pad),
                Item::Window => blit(&mut pm, &win, sl.x0 + item_pad),
                Item::Clock => blit(&mut pm, &clock, sl.x0 + item_pad),
                Item::Search => blit(&mut pm, &search, sl.x0 + item_pad),
                Item::Layout => blit(&mut pm, &lang, sl.x0 + item_pad),
                Item::Quick => {
                    let mut qx = sl.x0 + item_pad;
                    blit(&mut pm, &wifi, qx);
                    qx += wifi.as_ref().map(|p| p.width() as f32).unwrap_or(0.0) + gap;
                    blit(&mut pm, &vol, qx);
                    qx += vol.as_ref().map(|p| p.width() as f32).unwrap_or(0.0) + gap;
                    if let Some((pct, charging)) = self.battery {
                        draw_battery(&mut pm, qx, hf / 2.0, s, pct, charging, fgc, dark);
                    }
                }
            }
        }
        self.slots = slots;
        Some(pm)
    }

    fn item_at(&self, x: f32) -> Option<Item> {
        self.slots
            .iter()
            .find(|s| x >= s.x0 && x < s.x1)
            .map(|s| s.item)
    }

    fn slot(&self, item: Item) -> Option<Slot> {
        self.slots.iter().copied().find(|s| s.item == item)
    }
}

/// Berth's mark: three small tiles resting on a shelf.
fn draw_logo(pm: &mut Pixmap, sl: Slot, h: f32, s: f32, col: Color) {
    let cx = (sl.x0 + sl.x1) / 2.0;
    let cy = h / 2.0;
    let t = 4.2 * s;
    let g = 1.8 * s;
    let total = 3.0 * t + 2.0 * g;
    for i in 0..3 {
        let x = cx - total / 2.0 + i as f32 * (t + g);
        let lift = if i == 1 { 1.6 * s } else { 0.0 };
        if let Some(p) = draw::rounded_rect(RectF::new(x, cy - t - lift, t, t), 1.2 * s) {
            draw::fill(pm, &p, col);
        }
    }
    if let Some(p) = draw::rounded_rect(
        RectF::new(
            cx - total / 2.0 - 1.5 * s,
            cy + 1.6 * s,
            total + 3.0 * s,
            1.6 * s,
        ),
        0.8 * s,
    ) {
        draw::fill(pm, &p, col);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_battery(
    pm: &mut Pixmap,
    x: f32,
    cy: f32,
    s: f32,
    pct: u8,
    charging: bool,
    col: Color,
    dark: bool,
) {
    let (w, h) = (22.0 * s, 11.0 * s);
    let body = RectF::new(x, cy - h / 2.0, w, h);
    if let Some(p) = draw::rounded_rect(body, 3.0 * s) {
        let mut c = col;
        c.set_alpha(0.55);
        draw::stroke(pm, &p, c, 1.0 * s);
    }
    if let Some(p) = draw::rounded_rect(
        RectF::new(x + w + 1.0 * s, cy - 2.0 * s, 1.6 * s, 4.0 * s),
        0.8 * s,
    ) {
        let mut c = col;
        c.set_alpha(0.55);
        draw::fill(pm, &p, c);
    }
    let inner = body.inset(2.0 * s);
    let fw = inner.w * (pct.min(100) as f32 / 100.0);
    if let Some(p) = draw::rounded_rect(RectF::new(inner.x, inner.y, fw.max(1.0), inner.h), 1.5 * s)
    {
        let c = if charging {
            rgba(52, 199, 89, 1.0)
        } else if pct <= 20 {
            rgba(255, 59, 48, 1.0)
        } else {
            col
        };
        draw::fill(pm, &p, c);
    }
    let _ = dark;
}

fn clock_text() -> String {
    use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
    let mut date = [0u16; 64];
    let mut time = [0u16; 64];
    unsafe {
        let dn = GetDateFormatEx(
            PCWSTR::null(),
            Default::default(),
            None,
            w!("ddd d MMM"),
            Some(&mut date),
            PCWSTR::null(),
        );
        let tn = GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            None,
            PCWSTR::null(),
            Some(&mut time),
        );
        let d = String::from_utf16_lossy(&date[..(dn.max(1) - 1) as usize]);
        let t = String::from_utf16_lossy(&time[..(tn.max(1) - 1) as usize]);
        format!("{d}  {t}")
    }
}

/// The input language of the app in front ("EN"), when more than one
/// keyboard layout is installed.
fn input_language(fg: HWND) -> Option<String> {
    use windows::Win32::Globalization::LCIDToLocaleName;
    unsafe {
        if GetKeyboardLayoutList(None) <= 1 {
            return None;
        }
        let tid = if fg.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let hkl = GetKeyboardLayout(tid);
        let lang = (hkl.0 as usize & 0xffff) as u32;
        let mut buf = [0u16; 85];
        let n = LCIDToLocaleName(lang, Some(&mut buf), 0);
        if n <= 1 {
            return None;
        }
        let name = String::from_utf16_lossy(&buf[..(n - 1) as usize]);
        Some(name.split('-').next().unwrap_or("").to_ascii_uppercase())
    }
}

fn battery() -> Option<(u8, bool)> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut st = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut st).ok()? };
    if st.BatteryFlag & 128 != 0 || st.BatteryLifePercent > 100 {
        return None;
    }
    Some((st.BatteryLifePercent, st.ACLineStatus == 1))
}

/// Presses a Windows-key shortcut (e.g. Win+A opens Quick Settings).
fn win_shortcut(key: VIRTUAL_KEY) {
    let k = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        },
    };
    let seq = [
        k(VK_LWIN, false),
        k(key, false),
        k(key, true),
        k(VK_LWIN, true),
    ];
    unsafe {
        SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
    }
}

// ----- menus -------------------------------------------------------------------

const ID_ABOUT: u32 = 1;
const ID_SETTINGS: u32 = 2;
const ID_TASKMGR: u32 = 3;
const ID_DASHBOARD: u32 = 4;
const ID_SLEEP: u32 = 5;
const ID_RESTART: u32 = 6;
const ID_SHUTDOWN: u32 = 7;
const ID_LOCK: u32 = 8;
const ID_SIGNOUT: u32 = 9;
const ID_HIDE: u32 = 20;
const ID_HIDE_OTHERS: u32 = 21;
const ID_SHOW_ALL: u32 = 22;
const ID_QUIT: u32 = 23;
const ID_MIN: u32 = 30;
const ID_ZOOM: u32 = 31;
const ID_CLOSE: u32 = 32;
const ID_FRONT: u32 = 33;

/// Snapshot of what a menu acts on.
struct Target {
    hwnd: HWND,
    pid: u32,
    target: String,
    name: String,
    desktop: bool,
}

/// Top-level windows of the app (same program), front-most first.
fn app_windows_of(t: &Target) -> Vec<HWND> {
    if t.desktop {
        return Vec::new();
    }
    apps::app_windows(std::process::id())
        .into_iter()
        .filter(|w| {
            apps::window_app(w.hwnd)
                .map(|a| a.pid == t.pid || a.target.eq_ignore_ascii_case(&t.target))
                .unwrap_or(false)
        })
        .map(|w| w.hwnd)
        .collect()
}

fn open_menu(item: Item, x: i32, y: i32, t: Target) {
    use crate::{append, separator, track_menu};
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    let none = MENU_ITEM_FLAGS(0);
    unsafe {
        match item {
            Item::System => {
                append(menu, ID_ABOUT, "About This PC", none);
                append(menu, ID_SETTINGS, "Windows Settings\u{2026}", none);
                append(menu, ID_TASKMGR, "Task Manager", none);
                append(menu, ID_DASHBOARD, "Berth Dashboard\u{2026}", none);
                separator(menu);
                append(menu, ID_SLEEP, "Sleep", none);
                append(menu, ID_RESTART, "Restart\u{2026}", none);
                append(menu, ID_SHUTDOWN, "Shut Down\u{2026}", none);
                separator(menu);
                append(menu, ID_LOCK, "Lock Screen", none);
                append(menu, ID_SIGNOUT, "Sign Out\u{2026}", none);
            }
            Item::App => {
                let off = if t.desktop { MF_GRAYED } else { none };
                append(menu, ID_HIDE, &format!("Hide {}", t.name), off);
                append(menu, ID_HIDE_OTHERS, "Hide Others", none);
                append(menu, ID_SHOW_ALL, "Show All", none);
                separator(menu);
                append(menu, ID_QUIT, &format!("Quit {}", t.name), off);
            }
            Item::Window => {
                let off = if t.desktop { MF_GRAYED } else { none };
                append(menu, ID_MIN, "Minimize", off);
                append(menu, ID_ZOOM, "Zoom", off);
                append(menu, ID_CLOSE, "Close Window", off);
                separator(menu);
                append(menu, ID_FRONT, "Bring All to Front", off);
            }
            _ => {}
        }
    }
    let cmd = track_menu(menu, x, y);
    run_command(cmd, &t);
}

fn restore_focus(h: HWND) {
    if !h.0.is_null() {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
    }
}

fn run_command(cmd: u32, t: &Target) {
    let open = |s: &str| {
        let _ = apps::launch(s);
    };
    unsafe {
        match cmd {
            ID_ABOUT => open("ms-settings:about"),
            ID_SETTINGS => open("ms-settings:"),
            ID_TASKMGR => {
                let _ = std::process::Command::new("taskmgr.exe").spawn();
            }
            ID_DASHBOARD => crate::dashboard::open(),
            ID_SLEEP => {
                use windows::Win32::System::Power::SetSuspendState;
                let _ = SetSuspendState(false, false, false);
            }
            ID_RESTART => {
                if util::confirm_box(
                    None,
                    "Restart your PC now?\n\nOpen apps will be asked to close.",
                ) {
                    let _ = std::process::Command::new("shutdown.exe")
                        .args(["/r", "/t", "0"])
                        .spawn();
                } else {
                    restore_focus(t.hwnd);
                }
            }
            ID_SHUTDOWN => {
                if util::confirm_box(
                    None,
                    "Shut down your PC now?\n\nOpen apps will be asked to close.",
                ) {
                    let _ = std::process::Command::new("shutdown.exe")
                        .args(["/s", "/t", "0"])
                        .spawn();
                } else {
                    restore_focus(t.hwnd);
                }
            }
            ID_LOCK => {
                use windows::Win32::System::Shutdown::LockWorkStation;
                let _ = LockWorkStation();
            }
            ID_SIGNOUT => {
                if util::confirm_box(None, "Sign out now?\n\nOpen apps will be asked to close.") {
                    use windows::Win32::System::Shutdown::{
                        ExitWindowsEx, EWX_LOGOFF, SHUTDOWN_REASON,
                    };
                    crate::perform(crate::Action::Quit);
                    let _ = ExitWindowsEx(EWX_LOGOFF, SHUTDOWN_REASON(0));
                } else {
                    restore_focus(t.hwnd);
                }
            }
            ID_HIDE => {
                for h in app_windows_of(t) {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                }
            }
            ID_HIDE_OTHERS => {
                let mine = app_windows_of(t);
                for w in apps::app_windows(std::process::id()) {
                    if !mine.contains(&w.hwnd) {
                        let _ = ShowWindow(w.hwnd, SW_MINIMIZE);
                    }
                }
                restore_focus(t.hwnd);
            }
            ID_SHOW_ALL => {
                for w in apps::app_windows(std::process::id()) {
                    if IsIconic(w.hwnd).as_bool() {
                        let _ = ShowWindow(w.hwnd, SW_SHOWNOACTIVATE);
                    }
                }
                restore_focus(t.hwnd);
            }
            ID_QUIT => {
                for h in app_windows_of(t) {
                    let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            ID_MIN => {
                let _ = ShowWindow(t.hwnd, SW_MINIMIZE);
            }
            ID_ZOOM => {
                let _ = ShowWindow(
                    t.hwnd,
                    if IsZoomed(t.hwnd).as_bool() {
                        SW_RESTORE
                    } else {
                        SW_MAXIMIZE
                    },
                );
                restore_focus(t.hwnd);
            }
            ID_CLOSE => {
                let _ = PostMessageW(Some(t.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            ID_FRONT => {
                let wins = app_windows_of(t);
                for h in wins.iter().rev() {
                    if IsIconic(*h).as_bool() {
                        let _ = ShowWindow(*h, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(*h);
                }
                restore_focus(t.hwnd);
            }
            _ => restore_focus(t.hwnd),
        }
    }
}

fn click(x: f32) {
    let Some((item, slot, rect, target)) = with_mb(|b| {
        let item = b.item_at(x)?;
        let slot = b.slot(item)?;
        b.open = Some(item);
        b.paint();
        Some((
            item,
            slot,
            b.rect,
            Target {
                hwnd: b.app_hwnd,
                pid: b.app_pid,
                target: b.app_target.clone(),
                name: b.app_name.clone(),
                desktop: b.desktop,
            },
        ))
    })
    .flatten() else {
        return;
    };
    match item {
        Item::Search => win_shortcut(VK_S),
        Item::Layout => win_shortcut(VK_SPACE),
        Item::Quick => win_shortcut(VK_A),
        Item::Clock => win_shortcut(VK_N),
        _ => open_menu(item, rect.left + slot.x0 as i32, rect.bottom, target),
    }
    with_mb(|b| {
        b.open = None;
        b.hot = None;
        b.paint();
    });
}

/// `--self-test` and `--render-dashboard`: draws the bar (without showing
/// it) and checks that every item got a place.
pub fn render_png(path: &str, dark: bool, scale: f32) -> Result<bool, String> {
    let r = with_mb(|b| {
        b.dark = dark;
        b.scale = scale;
        b.clock = clock_text();
        b.battery = Some((76, false));
        b.layout = Some("EN".into());
        b.app_name = "File Explorer".into();
        let pm = b.render((1280.0 * scale) as i32, (28.0 * scale).round() as i32);
        let ok = b.slots.len() >= 7 && b.slots.iter().all(|s| s.x1 > s.x0);
        pm.map(|p| (p, ok))
    })
    .flatten();
    let Some((pm, ok)) = r else {
        return Err("menu bar not created".into());
    };
    pm.save_png(path).map_err(|e| e.to_string())?;
    Ok(ok)
}

/// CI only: registers the bar on the primary display and checks that
/// Windows reserves its strip, then gives it back.
pub fn appbar_test() -> (String, bool) {
    let Some(m) = system::pick_monitor("primary") else {
        return ("menu bar: no display".into(), false);
    };
    let before = m.work;
    sync(true, Some(m.clone()), false);
    std::thread::sleep(std::time::Duration::from_millis(600));
    let during = system::pick_monitor("primary").map(|m| m.work);
    let strip = reserved();
    sync(false, None, false);
    std::thread::sleep(std::time::Duration::from_millis(600));
    let after = system::pick_monitor("primary").map(|m| m.work);
    let reserved_ok = during.map(|d| d.top > before.top).unwrap_or(false) && strip.is_some();
    let restored = after == Some(before);
    (
        format!(
            "menu bar: work area {:?} -> {:?} -> {:?}; reserved: {reserved_ok}; given back: {restored}",
            (before.top, before.bottom),
            during.map(|d| (d.top, d.bottom)),
            after.map(|a| (a.top, a.bottom))
        ),
        reserved_ok && restored,
    )
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let x = (lp.0 & 0xffff) as i16 as f32;
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            if wp.0 == TIMER_TICK {
                with_mb(|b| b.refresh(false));
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with_mb(|b| {
                if !b.tracking {
                    b.tracking = true;
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                }
                let hot = b.item_at(x);
                if hot != b.hot {
                    b.hot = hot;
                    b.paint();
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE_ => {
            with_mb(|b| {
                b.tracking = false;
                if b.hot.is_some() {
                    b.hot = None;
                    b.paint();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            click(x);
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            if APPBAR.load(Ordering::Relaxed) == hwnd.0 as isize {
                let mut abd = APPBARDATA {
                    cbSize: std::mem::size_of::<APPBARDATA>() as u32,
                    hWnd: hwnd,
                    ..Default::default()
                };
                SHAppBarMessage(ABM_WINDOWPOSCHANGED, &mut abd);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_APPBAR => {
            match wp.0 as u32 {
                ABN_POSCHANGED => {
                    with_mb(|b| {
                        if b.enabled {
                            b.place();
                            b.refresh(true);
                        }
                    });
                }
                ABN_FULLSCREENAPP => {
                    with_mb(|b| {
                        b.fullscreen = lp.0 != 0;
                        b.paint();
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
