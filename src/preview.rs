//! Window previews: rest the pointer on the icon of an open app and a panel
//! above it shows a live thumbnail of each of its windows (drawn by Windows'
//! Desktop Window Manager), with the window's title. Click a thumbnail to
//! switch to that window, or its close button to close it.
//!
//! The panel is a separate top-most layered window that never takes focus.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY,
    DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Edge;
use crate::draw::{self, rgba, RectF};
use crate::icons::IconCache;
use crate::{text, with_app};

const CLASS: PCWSTR = w!("BerthPreview");
const TIMER_TICK: usize = 1;
const TICK_MS: u32 = 50;
/// How long the pointer rests on an icon before its windows are shown.
const SHOW_DELAY: Duration = Duration::from_millis(420);
/// How long the panel stays after the pointer moves away.
const HIDE_DELAY: Duration = Duration::from_millis(320);
const MAX_CARDS: usize = 8;

static OPEN: AtomicBool = AtomicBool::new(false);
static HOLD: AtomicBool = AtomicBool::new(false);

/// True while the panel is on screen.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// True while the pointer is on the panel (or on its way there), so the
/// dock keeps its hover state.
pub fn holds_hover() -> bool {
    OPEN.load(Ordering::Relaxed) && HOLD.load(Ordering::Relaxed)
}

struct Card {
    hwnd: HWND,
    minimized: bool,
    thumb: Option<isize>,
    /// Card, thumbnail area and close button, in panel pixels.
    rect: RectF,
    thumb_rect: RectF,
    close: RectF,
    title: Option<Pixmap>,
}

struct Preview {
    hwnd: HWND,
    timer: bool,
    /// The dock entry being shown, its windows and target.
    entry: Option<usize>,
    windows: Vec<HWND>,
    target: String,
    cards: Vec<Card>,
    pending: Option<(usize, Instant)>,
    leave_since: Option<Instant>,
    hot: Option<usize>,
    hot_close: bool,
    pos: RECT,
    dark: bool,
    scale: f32,
    icons: IconCache,
}

thread_local! {
    static PV: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

fn with_pv<R>(f: impl FnOnce(&mut Preview) -> R) -> Option<R> {
    PV.with(|p| p.try_borrow_mut().ok().and_then(|mut p| p.as_mut().map(f)))
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
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS,
            w!("Berth window previews"),
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
    };
    let Ok(hwnd) = hwnd else { return false };
    PV.with(|p| {
        *p.borrow_mut() = Some(Preview {
            hwnd,
            timer: false,
            entry: None,
            windows: Vec::new(),
            target: String::new(),
            cards: Vec::new(),
            pending: None,
            leave_since: None,
            hot: None,
            hot_close: false,
            pos: RECT::default(),
            dark: false,
            scale: 1.0,
            icons: IconCache::default(),
        })
    });
    true
}

/// The dock is being hovered: start watching for a resting pointer.
pub fn poke() {
    with_pv(|p| {
        if !p.timer {
            p.timer = true;
            unsafe { SetTimer(Some(p.hwnd), TIMER_TICK, TICK_MS, None) };
        }
    });
}

/// Hides the panel straight away (e.g. after a click on the dock).
pub fn hide() {
    with_pv(|p| p.hide());
}

/// What the dock reports on each tick.
struct DockState {
    enabled: bool,
    hovered: Option<usize>,
    windows: Vec<HWND>,
    target: String,
    icon: Option<RECT>,
    edge: Edge,
    work: RECT,
    scale: f32,
    dark: bool,
}

fn dock_state() -> Option<DockState> {
    with_app(|a| {
        let g = a.dock.geo.as_ref()?;
        let hovered = a.dock.hover_entry();
        let (windows, target) = hovered
            .and_then(|i| a.dock.entries.get(i))
            .map(|e| (e.windows.clone(), e.item.target.clone()))
            .unwrap_or_default();
        Some(DockState {
            enabled: a.settings.window_previews && a.dock.is_shown(),
            hovered,
            windows,
            target,
            icon: hovered.and_then(|i| a.dock.entry_screen_rect(i)),
            edge: g.edge,
            work: g.monitor.work,
            scale: g.scale,
            dark: a.dock.dark,
        })
    })
    .flatten()
}

fn cursor() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some(p)
}

impl Preview {
    fn tick(&mut self) {
        let Some(st) = dock_state() else {
            // The dock is busy (e.g. a menu is open); try again next tick.
            return;
        };
        let open = OPEN.load(Ordering::Relaxed);
        // Pointer on the panel (or the strip between it and the dock).
        let over = open
            && cursor()
                .map(|c| {
                    let m = (14.0 * self.scale) as i32;
                    c.x >= self.pos.left - m
                        && c.x < self.pos.right + m
                        && c.y >= self.pos.top - m
                        && c.y < self.pos.bottom + m
                })
                .unwrap_or(false);
        HOLD.store(over, Ordering::Relaxed);
        if !st.enabled {
            self.hide();
            self.stop_timer();
            return;
        }
        if over {
            self.leave_since = None;
            // Keep the panel in step with the app's windows.
            if let Some(e) = self.entry {
                let windows = with_app(|a| a.dock.entries.get(e).map(|x| x.windows.clone()))
                    .flatten()
                    .unwrap_or_default();
                if windows.is_empty() {
                    self.hide();
                } else if windows != self.windows {
                    self.windows = windows;
                    self.rebuild_in_place();
                }
            }
            return;
        }
        match st.hovered {
            Some(i) if !st.windows.is_empty() => {
                self.leave_since = None;
                if open && self.entry == Some(i) {
                    if st.windows != self.windows {
                        self.show(i, &st);
                    }
                } else if open {
                    self.show(i, &st);
                } else {
                    match self.pending {
                        Some((p, t)) if p == i => {
                            if t.elapsed() >= SHOW_DELAY {
                                self.pending = None;
                                self.show(i, &st);
                            }
                        }
                        _ => self.pending = Some((i, Instant::now())),
                    }
                }
            }
            _ => {
                self.pending = None;
                if open {
                    let since = *self.leave_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= HIDE_DELAY {
                        self.hide();
                    }
                } else if st.hovered.is_none() {
                    self.stop_timer();
                }
            }
        }
    }

    fn stop_timer(&mut self) {
        if self.timer {
            self.timer = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
    }

    fn unregister_thumbs(&mut self) {
        for c in &mut self.cards {
            if let Some(t) = c.thumb.take() {
                unsafe {
                    let _ = DwmUnregisterThumbnail(t);
                }
            }
        }
    }

    fn hide(&mut self) {
        self.unregister_thumbs();
        self.cards.clear();
        self.entry = None;
        self.windows.clear();
        self.hot = None;
        self.hot_close = false;
        self.leave_since = None;
        if OPEN.swap(false, Ordering::Relaxed) {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        HOLD.store(false, Ordering::Relaxed);
    }

    fn show(&mut self, entry: usize, st: &DockState) {
        let Some(icon) = st.icon else { return };
        self.unregister_thumbs();
        self.entry = Some(entry);
        self.windows = st.windows.clone();
        self.target = st.target.clone();
        self.dark = st.dark;
        self.scale = st.scale;
        self.hot = None;
        self.hot_close = false;
        self.layout_and_place(Some((icon, st.edge, st.work)));
    }

    /// Re-lays out for a changed window list, keeping the anchor.
    fn rebuild_in_place(&mut self) {
        self.unregister_thumbs();
        self.hot = None;
        self.layout_and_place(None);
    }

    fn layout_and_place(&mut self, anchor: Option<(RECT, Edge, RECT)>) {
        let s = self.scale;
        let shadow = (14.0 * s).round();
        let outer = (8.0 * s).round();
        let gap = (6.0 * s).round();
        let cpad = (8.0 * s).round();
        let title_h = (24.0 * s).round();
        let (edge, work) = match anchor {
            Some((_, e, w)) => (e, w),
            None => (self.edge_cache(), self.work_cache()),
        };
        let vertical = edge != Edge::Bottom;
        let windows: Vec<HWND> = self.windows.iter().copied().take(MAX_CARDS).collect();
        let n = windows.len().max(1) as f32;
        // Thumbnail box, shrunk if the cards don't fit along the screen.
        let (mut tw, mut th) = ((208.0 * s).round(), (130.0 * s).round());
        let avail = if vertical {
            (work.bottom - work.top) as f32
        } else {
            (work.right - work.left) as f32
        } - 2.0 * (shadow + outer);
        let per = if vertical {
            th + 2.0 * cpad + title_h + gap
        } else {
            tw + 2.0 * cpad + gap
        };
        if n * per > avail {
            let k = (avail / (n * per)).max(0.45);
            tw = (tw * k).round();
            th = (th * k).round();
        }
        let cw = tw + 2.0 * cpad;
        let ch = th + 2.0 * cpad + title_h;
        let (pw, ph) = if vertical {
            (cw + 2.0 * outer, n * ch + (n - 1.0) * gap + 2.0 * outer)
        } else {
            (n * cw + (n - 1.0) * gap + 2.0 * outer, ch + 2.0 * outer)
        };
        let (ww, wh) = (pw + 2.0 * shadow, ph + 2.0 * shadow);

        // Place next to the icon, away from the screen edge.
        let (x, y) = if let Some((icon, edge, work)) = anchor {
            let away = (10.0 * s).round() as i32;
            let (cx, cy) = ((icon.left + icon.right) / 2, (icon.top + icon.bottom) / 2);
            let (mut x, mut y) = match edge {
                Edge::Bottom => (cx - ww as i32 / 2, icon.top - away - (wh - shadow) as i32),
                Edge::Left => (icon.right + away - shadow as i32, cy - wh as i32 / 2),
                Edge::Right => (icon.left - away - (ww - shadow) as i32, cy - wh as i32 / 2),
            };
            // Magnified icons grow away from the edge; stay clear of them.
            let grow = ((icon.bottom - icon.top) as f32 * 0.6) as i32;
            match edge {
                Edge::Bottom => y -= grow,
                Edge::Left => x += grow,
                Edge::Right => x -= grow,
            }
            x = x.clamp(
                work.left - shadow as i32,
                (work.right - ww as i32 + shadow as i32).max(work.left),
            );
            y = y.clamp(
                work.top - shadow as i32,
                (work.bottom - wh as i32 + shadow as i32).max(work.top),
            );
            (x, y)
        } else {
            (self.pos.left - shadow as i32, self.pos.top - shadow as i32)
        };
        self.pos = RECT {
            left: x + shadow as i32,
            top: y + shadow as i32,
            right: x + (shadow + pw) as i32,
            bottom: y + (shadow + ph) as i32,
        };
        EDGE_CACHE.with(|c| *c.borrow_mut() = (edge, work));

        // Cards.
        self.cards = windows
            .iter()
            .enumerate()
            .map(|(i, &h)| {
                let i = i as f32;
                let (cx, cy) = if vertical {
                    (shadow + outer, shadow + outer + i * (ch + gap))
                } else {
                    (shadow + outer + i * (cw + gap), shadow + outer)
                };
                let rect = RectF::new(cx, cy, cw, ch);
                let thumb_rect = RectF::new(cx + cpad, cy + cpad, tw, th);
                let cs = (18.0 * s).round();
                let close = RectF::new(
                    cx + cw - cpad - cs,
                    cy + cpad + th + (title_h - cs) / 2.0 + 2.0 * s,
                    cs,
                    cs,
                );
                Card {
                    hwnd: h,
                    minimized: unsafe { IsIconic(h).as_bool() },
                    thumb: None,
                    rect,
                    thumb_rect,
                    close,
                    title: None,
                }
            })
            .collect();
        let title_px = (12.0 * s).round() as i32;
        let color = if self.dark {
            (240, 240, 244)
        } else {
            (26, 26, 30)
        };
        for c in &mut self.cards {
            let t = window_title(c.hwnd);
            let max_w = (c.rect.w - 2.0 * cpad - c.close.w - 6.0 * s) as i32;
            c.title = text::render(&t, title_px, max_w.max(20), color);
        }

        self.paint(ww as i32, wh as i32, x, y);
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
        OPEN.store(true, Ordering::Relaxed);
        self.place_thumbs();
    }

    fn edge_cache(&self) -> Edge {
        EDGE_CACHE.with(|c| c.borrow().0)
    }
    fn work_cache(&self) -> RECT {
        EDGE_CACHE.with(|c| c.borrow().1)
    }

    /// Registers a live DWM thumbnail for each window that isn't minimized.
    fn place_thumbs(&mut self) {
        for c in &mut self.cards {
            // Windows keeps a picture of minimized windows too; the app icon
            // drawn underneath shows only if it has none.
            let Ok(t) = (unsafe { DwmRegisterThumbnail(self.hwnd, c.hwnd) }) else {
                continue;
            };
            let src = unsafe { DwmQueryThumbnailSourceSize(t) }.unwrap_or(SIZE { cx: 16, cy: 10 });
            let (sw, sh) = (src.cx.max(1) as f32, src.cy.max(1) as f32);
            let k = (c.thumb_rect.w / sw).min(c.thumb_rect.h / sh);
            let (dw, dh) = (sw * k, sh * k);
            let dx = c.thumb_rect.x + (c.thumb_rect.w - dw) / 2.0;
            let dy = c.thumb_rect.y + (c.thumb_rect.h - dh) / 2.0;
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION
                    | DWM_TNP_VISIBLE
                    | DWM_TNP_OPACITY
                    | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT {
                    left: dx.round() as i32,
                    top: dy.round() as i32,
                    right: (dx + dw).round() as i32,
                    bottom: (dy + dh).round() as i32,
                },
                opacity: 255,
                fVisible: BOOL(1),
                fSourceClientAreaOnly: BOOL(0),
                ..Default::default()
            };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(t, &props);
            }
            c.thumb = Some(t);
        }
    }

    fn paint(&mut self, w: i32, h: i32, x: i32, y: i32) {
        let (Some(mut pm), Some(mut scratch)) = (
            Pixmap::new(w as u32, h as u32),
            Pixmap::new(w as u32, h as u32),
        ) else {
            return;
        };
        let s = self.scale;
        let dark = self.dark;
        let panel = RectF::new(
            (self.pos.left - x) as f32,
            (self.pos.top - y) as f32,
            (self.pos.right - self.pos.left) as f32,
            (self.pos.bottom - self.pos.top) as f32,
        );
        let radius = 14.0 * s;
        draw::soft_shadow(
            &mut pm,
            &mut scratch,
            panel,
            radius,
            12.0 * s,
            (0.0, 3.0 * s),
            if dark { 0.6 } else { 0.3 },
        );
        if let Some(p) = draw::rounded_rect(panel, radius) {
            let bg = if dark {
                rgba(36, 36, 40, 0.96)
            } else {
                rgba(248, 248, 250, 0.97)
            };
            draw::fill(&mut pm, &p, bg);
            draw::stroke(
                &mut pm,
                &p,
                if dark {
                    rgba(255, 255, 255, 0.12)
                } else {
                    rgba(0, 0, 0, 0.10)
                },
                1.0,
            );
        }
        let icon_px = (48.0 * s).round() as u32;
        let target = self.target.clone();
        for (i, c) in self.cards.iter().enumerate() {
            let hot = self.hot == Some(i);
            if hot {
                if let Some(p) = draw::rounded_rect(c.rect, 8.0 * s) {
                    draw::fill(
                        &mut pm,
                        &p,
                        if dark {
                            rgba(255, 255, 255, 0.09)
                        } else {
                            rgba(0, 0, 0, 0.06)
                        },
                    );
                }
            }
            // Thumbnail backdrop (the live thumbnail is drawn over it).
            if let Some(p) = draw::rounded_rect(c.thumb_rect, 4.0 * s) {
                draw::fill(
                    &mut pm,
                    &p,
                    if dark {
                        rgba(255, 255, 255, 0.04)
                    } else {
                        rgba(0, 0, 0, 0.04)
                    },
                );
            }
            if c.minimized {
                if let Some(icon) = self.icons.get(&target, icon_px) {
                    let ix = c.thumb_rect.cx() - icon.width() as f32 / 2.0;
                    let iy = c.thumb_rect.cy() - icon.height() as f32 / 2.0;
                    pm.draw_pixmap(
                        ix.round() as i32,
                        iy.round() as i32,
                        icon.as_ref(),
                        &PixmapPaint {
                            opacity: 0.85,
                            quality: FilterQuality::Bicubic,
                            ..PixmapPaint::default()
                        },
                        Transform::identity(),
                        None,
                    );
                }
            }
            if let Some(t) = &c.title {
                let ty = c.thumb_rect.y + c.thumb_rect.h + 6.0 * s;
                pm.draw_pixmap(
                    c.thumb_rect.x.round() as i32,
                    ty.round() as i32,
                    t.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
            if hot {
                let r = c.close;
                if let Some(p) = draw::circle(r.cx(), r.cy(), r.w / 2.0) {
                    let fill = if self.hot_close {
                        rgba(232, 17, 35, 1.0)
                    } else if dark {
                        rgba(255, 255, 255, 0.14)
                    } else {
                        rgba(0, 0, 0, 0.09)
                    };
                    draw::fill(&mut pm, &p, fill);
                }
                let k = r.w * 0.2;
                let col = if self.hot_close || dark {
                    Color::WHITE
                } else {
                    rgba(30, 30, 34, 1.0)
                };
                let mut pb = tiny_skia::PathBuilder::new();
                pb.move_to(r.cx() - k, r.cy() - k);
                pb.line_to(r.cx() + k, r.cy() + k);
                pb.move_to(r.cx() + k, r.cy() - k);
                pb.line_to(r.cx() - k, r.cy() + k);
                if let Some(path) = pb.finish() {
                    draw::stroke(&mut pm, &path, col, (1.4 * s).max(1.0));
                }
            }
        }
        push_layered(self.hwnd, &pm, x, y);
    }

    fn repaint(&mut self) {
        let shadow = (14.0 * self.scale).round() as i32;
        let x = self.pos.left - shadow;
        let y = self.pos.top - shadow;
        let w = self.pos.right - self.pos.left + 2 * shadow;
        let h = self.pos.bottom - self.pos.top + 2 * shadow;
        self.paint(w, h, x, y);
    }

    fn hit(&self, x: f32, y: f32) -> (Option<usize>, bool) {
        for (i, c) in self.cards.iter().enumerate() {
            let inside = |r: &RectF| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
            if inside(&c.rect) {
                return (Some(i), inside(&c.close));
            }
        }
        (None, false)
    }

    fn on_move(&mut self, x: f32, y: f32) {
        let (hot, close) = self.hit(x, y);
        if hot != self.hot || close != self.hot_close {
            self.hot = hot;
            self.hot_close = close;
            self.repaint();
        }
    }

    /// Returns the window to switch to, if a thumbnail was clicked.
    fn on_click(&mut self, x: f32, y: f32) -> Option<HWND> {
        let (hot, close) = self.hit(x, y);
        let c = self.cards.get(hot?)?;
        if close {
            unsafe {
                let _ = PostMessageW(Some(c.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            None
        } else {
            Some(c.hwnd)
        }
    }
}

/// `--self-test`: checks that Windows accepts a live thumbnail on the
/// preview panel (using the taskbar as the source window).
pub fn self_test() -> (String, bool) {
    let Some(dest) = with_pv(|p| p.hwnd) else {
        return ("window previews: panel not created".into(), false);
    };
    let Ok(src) = (unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }) else {
        return (
            "window previews: no taskbar to test with (skipped)".into(),
            true,
        );
    };
    match unsafe { DwmRegisterThumbnail(dest, src) } {
        Ok(t) => {
            let size = unsafe { DwmQueryThumbnailSourceSize(t) };
            unsafe {
                let _ = DwmUnregisterThumbnail(t);
            }
            let ok = size.map(|s| s.cx > 0 && s.cy > 0).unwrap_or(false);
            (
                format!(
                    "window previews: live thumbnail {}",
                    if ok { "ok" } else { "FAILED (no size)" }
                ),
                ok,
            )
        }
        Err(e) => (format!("window previews: thumbnail FAILED ({e})"), false),
    }
}

thread_local! {
    static EDGE_CACHE: RefCell<(Edge, RECT)> = const { RefCell::new((Edge::Bottom, RECT { left: 0, top: 0, right: 0, bottom: 0 })) };
}

fn window_title(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    let t = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if t.trim().is_empty() {
        "Untitled".into()
    } else {
        t
    }
}

/// Pushes a premultiplied RGBA pixmap to a layered window at (x, y).
pub fn push_layered(hwnd: HWND, pm: &Pixmap, x: i32, y: i32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                let old = SelectObject(dc, HGDIOBJ(dib.0));
                let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
                for (d, s) in dst.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
                let pos = POINT { x, y };
                let size = SIZE { cx: w, cy: h };
                let src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: 0,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: 1,
                };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    None,
                    Some(&pos),
                    Some(&size),
                    Some(dc),
                    Some(&src),
                    windows::Win32::Foundation::COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                SelectObject(dc, old);
            }
            let _ = DeleteObject(HGDIOBJ(dib.0));
        }
        let _ = DeleteDC(dc);
    }
}

fn switch_to(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = (
        (lp.0 & 0xffff) as i16 as f32,
        ((lp.0 >> 16) & 0xffff) as i16 as f32,
    );
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            if wp.0 == TIMER_TICK {
                with_pv(|p| p.tick());
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with_pv(|p| p.on_move(x, y));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(Some(h)) = with_pv(|p| p.on_click(x, y)) {
                with_pv(|p| p.hide());
                switch_to(h);
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            // Middle-click closes a window, as on the taskbar.
            if let Some((Some(i), _)) = with_pv(|p| p.hit(x, y)) {
                if let Some(Some(h)) = with_pv(|p| p.cards.get(i).map(|c| c.hwnd)) {
                    let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
