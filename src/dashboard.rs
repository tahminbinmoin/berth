//! The Berth Dashboard: one window to control everything, drawn in the
//! style of Windows 11 Settings (sidebar navigation, rounded cards, toggle
//! switches, sliders and dropdowns), following the light/dark theme and the
//! Windows accent colour. Every change applies to the dock immediately and
//! is saved automatically.
//!
//! The window is drawn with tiny-skia through the `widgets` toolkit; only
//! the two text boxes (app search and rename) are native edit controls.

use std::cell::RefCell;
use std::collections::HashSet;

use tiny_skia::Pixmap;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, InvalidateRect, SetBkColor,
    SetDIBitsToDevice, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBRUSH,
    HDC, HFONT, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{AnimationMode, Edge, HoverEffect, PinnedItem, Settings, ThemeMode};
use crate::draw::RectF;
use crate::icons::IconCache;
use crate::widgets::{id, idx, Font, Input, Key, Palette, TextCache, Ui, UiState};
use crate::{apps, autostart, picker, system, ui, util, with_app, Action};

const CLASS: PCWSTR = w!("BerthDashboard2");
const WM_APP_DASH_REFRESH: u32 = WM_APP + 40;
const WM_APP_FRAME: u32 = WM_APP + 41;
const WM_APP_EDIT_DONE: u32 = WM_APP + 42;
const TIMER_ICONS: usize = 1;
const MOUSELEAVE: u32 = 0x02A3;

const W: f32 = 1000.0;
const H: f32 = 680.0;
const SIDEBAR: f32 = 284.0;
const X0: f32 = 304.0;
const CW: f32 = 664.0;
const TOP: f32 = 86.0;

pub const UPDATES_PAGE: usize = 4;
const APPS_PAGE: usize = 3;
const PAGES: [(&str, char); 6] = [
    ("General", '\u{E713}'),
    ("Appearance", '\u{E790}'),
    ("Behavior", '\u{E7F4}'),
    ("Apps", '\u{E71D}'),
    ("Updates", '\u{E895}'),
    ("About", '\u{E946}'),
];

// ----- state -----------------------------------------------------------------

enum Cmd {
    Set(Box<dyn FnOnce(&mut Settings)>),
    Autostart(bool),
    ShowDock(bool),
    Restore,
    OpenFolder,
    Quit,
    CheckUpdates,
    OpenUrl(String),
    OpenSheet,
    CloseSheet,
    SheetAdd,
    Browse(bool),
    StartRename(usize),
    Move(usize, bool),
    Remove(usize),
}

struct Sheet {
    apps: Vec<apps::InstalledApp>,
    shown: Vec<usize>,
    selected: HashSet<String>,
    scroll: f32,
    query: String,
}

struct Snap {
    s: Settings,
    hidden: bool,
    auto: autostart::State,
    displays: Vec<(String, String)>,
}

struct Dash {
    hwnd: HWND,
    dpi: u32,
    pm: Option<Pixmap>,
    bgra: Vec<u8>,
    st: UiState,
    text: TextCache,
    input: Input,
    page: usize,
    scroll: [f32; 6],
    content_h: [f32; 6],
    icons: IconCache,
    icons_pending: bool,
    sheet: Option<Sheet>,
    rename: Option<usize>,
    search_edit: HWND,
    rename_edit: HWND,
    edit_font: HFONT,
    edit_brush: HBRUSH,
    edit_colors: (COLORREF, COLORREF),
    last_dark: Option<bool>,
    accent: (Option<(u8, u8, u8)>, Option<(u8, u8, u8)>),
    logo: Option<Pixmap>,
    displays: Vec<(String, String)>,
    tracking: bool,
}

thread_local! {
    static UPDATE_STATUS: RefCell<(String, bool)> = const { RefCell::new((String::new(), false)) };
    static DASH: RefCell<Option<Dash>> = const { RefCell::new(None) };
}

fn with_dash<R>(f: impl FnOnce(&mut Dash) -> R) -> Option<R> {
    DASH.with(|d| {
        let mut b = d.try_borrow_mut().ok()?;
        Some(f(b.as_mut()?))
    })
}

fn hwnd() -> Option<HWND> {
    with_dash(|d| d.hwnd)
}

/// Windows accent palette: (for dark theme, for light theme).
fn read_accent() -> (Option<(u8, u8, u8)>, Option<(u8, u8, u8)>) {
    let mut buf = [0u8; 32];
    let mut size = buf.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent"),
            &HSTRING::from("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r.is_err() || size < 32 {
        return (None, None);
    }
    let at = |i: usize| Some((buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2]));
    (at(1), at(4))
}

fn monitors_list() -> Vec<(String, String)> {
    let mut v = vec![("Main display".to_string(), "primary".to_string())];
    for (i, m) in system::monitors().iter().enumerate() {
        v.push((m.label(i), m.device.clone()));
    }
    v
}

// ----- public API --------------------------------------------------------------

pub fn register(hinst: HINSTANCE) -> bool {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc_),
        hInstance: hinst,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        hIcon: crate::tray::app_icon(32),
        hIconSm: crate::tray::app_icon(16),
        lpszClassName: CLASS,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) != 0 }
}

pub fn open() {
    open_page(0);
}

/// Opens the dashboard on the Apps page with the "Add apps" panel.
pub fn open_add_apps() {
    open_page(APPS_PAGE);
    run_cmds(vec![Cmd::OpenSheet]);
}

pub fn open_page(page: usize) {
    open_page_inner(page, true);
}

/// Developer aid (`--render-dashboard <dir>`): draws every page in light
/// and dark, plus the Add apps panel, to PNG files.
pub fn render_to_files(dir: &str) -> Result<usize, String> {
    open_page_inner(0, false);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut n = 0;
    for dark in [false, true] {
        with_app(|a| {
            a.settings.theme = if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            }
        });
        for page in 0..PAGES.len() {
            with_dash(|d| {
                d.page = page;
                d.last_dark = None;
            });
            frame();
            frame();
            let name = format!(
                "{dir}\\dashboard-{}-{}.png",
                page,
                if dark { "dark" } else { "light" }
            );
            with_dash(|d| d.pm.as_ref().map(|p| p.save_png(&name)))
                .flatten()
                .transpose()
                .map_err(|e| e.to_string())?;
            n += 1;
        }
        with_dash(|d| {
            d.page = APPS_PAGE;
            d.sheet = Some(Sheet {
                apps: apps::installed_apps(),
                shown: Vec::new(),
                selected: HashSet::new(),
                scroll: 0.0,
                query: String::new(),
            });
            if let Some(sh) = d.sheet.as_mut() {
                filter_sheet(sh);
                if let Some(first) = sh.apps.first() {
                    sh.selected.insert(first.target.clone());
                }
            }
        });
        for _ in 0..30 {
            frame();
        }
        let name = format!(
            "{dir}\\dashboard-sheet-{}.png",
            if dark { "dark" } else { "light" }
        );
        with_dash(|d| d.pm.as_ref().map(|p| p.save_png(&name)))
            .flatten()
            .transpose()
            .map_err(|e| e.to_string())?;
        with_dash(|d| d.sheet = None);
        n += 1;
    }
    Ok(n)
}

fn open_page_inner(page: usize, show: bool) {
    if let Some(h) = hwnd() {
        with_dash(|d| d.page = page.min(PAGES.len() - 1));
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
        request_frame();
        return;
    }
    let (work, dpi) = with_app(|a| a.dock.geo.as_ref().map(|g| (g.monitor.work, g.monitor.dpi)))
        .flatten()
        .or_else(|| system::pick_monitor("primary").map(|m| (m.work, m.dpi)))
        .unwrap_or((
            RECT {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 800,
            },
            96,
        ));
    let style = (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX).0;
    let r = ui::centered_window_rect(work, W as i32, H as i32, dpi as f32 / 96.0, style, 0, dpi);
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Berth Dashboard"),
            WINDOW_STYLE(style),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            None,
            None,
        )
    };
    let Ok(h) = created else {
        util::error_box(None, "Berth couldn't open the dashboard window.");
        return;
    };
    let dpi = ui::dpi_of(h);
    let edit = |id: isize| unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | ES_AUTOHSCROLL as u32),
            0,
            0,
            10,
            10,
            Some(h),
            Some(HMENU(id as *mut _)),
            None,
            None,
        )
        .unwrap_or_default()
    };
    let search = edit(10);
    let rename = edit(11);
    unsafe {
        let _ = SetWindowSubclass(search, Some(edit_proc), 1, 0);
        let _ = SetWindowSubclass(rename, Some(edit_proc), 2, 0);
    }
    let logo = unsafe {
        let icon = crate::tray::app_icon(256);
        if icon.is_invalid() {
            None
        } else {
            crate::icons::hicon_to_pixmap(icon)
        }
    };
    DASH.with(|d| {
        *d.borrow_mut() = Some(Dash {
            hwnd: h,
            dpi,
            pm: None,
            bgra: Vec::new(),
            st: UiState::default(),
            text: TextCache::default(),
            input: Input::default(),
            page: page.min(PAGES.len() - 1),
            scroll: [0.0; 6],
            content_h: [0.0; 6],
            icons: IconCache::default(),
            icons_pending: false,
            sheet: None,
            rename: None,
            search_edit: search,
            rename_edit: rename,
            edit_font: HFONT::default(),
            edit_brush: HBRUSH::default(),
            edit_colors: (COLORREF(0), COLORREF(0)),
            last_dark: None,
            accent: read_accent(),
            logo,
            displays: monitors_list(),
            tracking: false,
        })
    });
    update_edit_font();
    frame();
    if show {
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        }
    }
}

pub fn close() {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

pub fn window() -> Option<HWND> {
    hwnd()
}

/// Re-reads settings (safe to call while the app state is borrowed).
pub fn notify_changed() {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = PostMessageW(Some(h), WM_APP_DASH_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn refresh() {
    notify_changed();
}

/// The dashboard is not a dialog; nothing to pre-translate.
pub fn dialog_message(_msg: &MSG) -> bool {
    false
}

pub fn set_update_status(text: &str, busy: bool) {
    UPDATE_STATUS.with(|s| *s.borrow_mut() = (text.to_string(), busy));
    request_frame();
}

/// First launch: open the dashboard and ask about starting with Windows.
pub fn first_run_welcome() {
    open_page(0);
    let owner = hwnd();
    let yes = util::ask_box(
        owner,
        "Welcome to Berth!\n\nStart Berth automatically in the background whenever you sign in to Windows?\n\n\
         You can change this at any time on the General page of this dashboard, or in Windows Settings > Apps > Startup.\n\n\
         Berth will also let you know when a new version is out (Dashboard > Updates).",
    );
    if yes {
        if let Err(e) = autostart::enable() {
            util::error_box(
                owner,
                &format!("Berth couldn't turn on launch at sign-in.\n\n{e}"),
            );
        }
    }
    notify_changed();
}

fn request_frame() {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = PostMessageW(Some(h), WM_APP_FRAME, WPARAM(0), LPARAM(0));
        }
    }
}

// ----- frame ---------------------------------------------------------------------

fn snapshot(displays: &[(String, String)]) -> Option<Snap> {
    with_app(|a| Snap {
        s: a.settings.clone(),
        hidden: a.dock.user_hidden,
        auto: autostart::state(),
        displays: displays.to_vec(),
    })
}

/// Runs one UI frame with the pending input, presents it, then executes
/// any resulting commands.
fn frame() {
    let Some(displays) = with_dash(|d| d.displays.clone()) else {
        return;
    };
    let Some(snap) = snapshot(&displays) else {
        request_frame();
        return;
    };
    let (status, busy) = UPDATE_STATUS.with(|s| s.borrow().clone());
    let out = with_dash(|d| {
        let cmds = d.run(&snap, &status, busy);
        let again = d.st.want_frame;
        d.st.want_frame = false;
        d.input.pressed = false;
        d.input.released = false;
        d.input.wheel = 0.0;
        d.input.key = None;
        (cmds, again, d.hwnd)
    });
    let Some((cmds, again, h)) = out else { return };
    unsafe {
        let _ = InvalidateRect(Some(h), None, false);
    }
    position_edits();
    run_cmds(cmds);
    if again {
        request_frame();
    }
}

impl Dash {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn run(&mut self, snap: &Snap, status: &str, busy: bool) -> Vec<Cmd> {
        let s = self.scale();
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (pw, ph) = (
            (rc.right - rc.left).max(1) as u32,
            (rc.bottom - rc.top).max(1) as u32,
        );
        if self.pm.as_ref().map(|p| (p.width(), p.height())) != Some((pw, ph)) {
            self.pm = Pixmap::new(pw, ph);
        }
        let dark = system::is_dark(snap.s.theme);
        if self.last_dark != Some(dark) {
            self.last_dark = Some(dark);
            self.text.clear();
            apply_title_bar(self.hwnd, dark);
            self.refresh_edit_colors(dark);
        }
        let accent = if dark { self.accent.0 } else { self.accent.1 };
        let pal = Palette::new(dark, accent);
        let mut cmds = Vec::new();
        let Some(mut pm) = self.pm.take() else {
            return cmds;
        };
        {
            let modal = self.sheet.is_some();
            let mut ui = Ui::new(&mut pm, s, pal, &self.input, &mut self.st, &mut self.text);
            let dropdown_open = ui.blocked;
            ui.blocked = dropdown_open || modal;
            if ui.input.pressed {
                ui.st.hot = None;
            }
            sidebar(&mut ui, &mut self.page, self.logo.as_ref());
            let page = self.page;
            let area = RectF::new(X0 - 4.0, TOP, W - X0, H - TOP);
            ui.set_clip(Some(area));
            ui.dy = self.scroll[page];
            let title_y = 26.0;
            let end = {
                let (dy, clip_guard) = (ui.dy, ());
                let _ = clip_guard;
                ui.dy = 0.0;
                ui.set_clip(None);
                ui.text_at(
                    PAGES[page].0,
                    X0,
                    title_y,
                    Font::Title,
                    28.0,
                    ui.p.text,
                    CW,
                    false,
                );
                ui.set_clip(Some(area));
                ui.dy = dy;
                match page {
                    0 => page_general(&mut ui, snap, &mut cmds),
                    1 => page_appearance(&mut ui, snap, &mut cmds),
                    2 => page_behavior(&mut ui, snap, &mut cmds),
                    3 => page_apps(
                        &mut ui,
                        snap,
                        &mut cmds,
                        &mut self.icons,
                        &mut self.icons_pending,
                        self.rename,
                    ),
                    4 => page_updates(&mut ui, snap, &mut cmds, status, busy),
                    _ => page_about(&mut ui, &mut cmds, self.logo.as_ref()),
                }
            };
            self.content_h[page] = end - TOP + 24.0;
            ui.set_clip(None);
            ui.dy = 0.0;
            self.scroll[page] = ui.scroll_area(area, self.content_h[page], self.scroll[page]);
            ui.blocked = dropdown_open;
            if let Some(sheet) = self.sheet.as_mut() {
                sheet_ui(
                    &mut ui,
                    sheet,
                    &mut cmds,
                    &mut self.icons,
                    &mut self.icons_pending,
                );
            }
            ui.finish();
        }
        // Presentation buffer (BGRA).
        let data = pm.data();
        if self.bgra.len() != data.len() {
            self.bgra = vec![0; data.len()];
        }
        for (d, src) in self.bgra.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
            d[0] = src[2];
            d[1] = src[1];
            d[2] = src[0];
            d[3] = 255;
        }
        self.pm = Some(pm);
        if self.icons_pending {
            unsafe { SetTimer(Some(self.hwnd), TIMER_ICONS, 30, None) };
        }
        cmds
    }

    fn refresh_edit_colors(&mut self, dark: bool) {
        let pal = Palette::new(dark, None);
        let (r, g, b) = Palette::rgb(pal.input);
        let (tr, tg, tb) = Palette::rgb(pal.text);
        if !self.edit_brush.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.edit_brush.0));
            }
        }
        let bg = COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16);
        let fg = COLORREF(tr as u32 | (tg as u32) << 8 | (tb as u32) << 16);
        self.edit_brush = unsafe { CreateSolidBrush(bg) };
        self.edit_colors = (bg, fg);
        unsafe {
            let _ = InvalidateRect(Some(self.search_edit), None, true);
            let _ = InvalidateRect(Some(self.rename_edit), None, true);
        }
    }
}

fn apply_title_bar(h: HWND, dark: bool) {
    let pal = Palette::new(dark, None);
    let (r, g, b) = Palette::rgb(pal.bg);
    let caption = r as u32 | (g as u32) << 8 | (b as u32) << 16;
    let on: i32 = dark as i32;
    unsafe {
        // DWMWA_USE_IMMERSIVE_DARK_MODE = 20, DWMWA_CAPTION_COLOR = 35
        let _ = DwmSetWindowAttribute(h, DWMWINDOWATTRIBUTE(20), &on as *const i32 as *const _, 4);
        let _ = DwmSetWindowAttribute(
            h,
            DWMWINDOWATTRIBUTE(35),
            &caption as *const u32 as *const _,
            4,
        );
    }
}

fn update_edit_font() {
    with_dash(|d| {
        let px = (14.0 * d.scale()).round() as i32;
        if !d.edit_font.is_invalid() {
            ui::delete_font(d.edit_font);
        }
        d.edit_font = unsafe {
            windows::Win32::Graphics::Gdi::CreateFontW(
                -px,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                windows::Win32::Graphics::Gdi::DEFAULT_CHARSET,
                windows::Win32::Graphics::Gdi::OUT_TT_PRECIS,
                windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS,
                windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI Variable Text"),
            )
        };
        for e in [d.search_edit, d.rename_edit] {
            ui::send(e, WM_SETFONT, d.edit_font.0 as usize, 1);
        }
    });
}

// ----- layout helpers ----------------------------------------------------------------

const ROW: f32 = 68.0;

/// Draws a card holding `rows` rows of the given heights starting at `y`;
/// returns the y of each row and the y after the card.
fn card_rows(ui: &mut Ui, y: f32, heights: &[f32]) -> (Vec<f32>, f32) {
    let total: f32 = heights.iter().sum();
    ui.card(RectF::new(X0, y, CW, total));
    let mut ys = Vec::new();
    let mut cy = y;
    for (i, h) in heights.iter().enumerate() {
        if i > 0 {
            ui.divider(X0 + 1.0, cy, CW - 2.0);
        }
        ys.push(cy);
        cy += h;
    }
    (ys, cy + 8.0)
}

fn right() -> f32 {
    X0 + CW - 20.0
}

fn label(ui: &mut Ui, y: f32, h: f32, title: &str, desc: &str, enabled: bool) {
    ui.row_label(X0 + 20.0, y, h, CW - 340.0, title, desc, enabled);
}

fn section(ui: &mut Ui, y: f32, text: &str) -> f32 {
    ui.text_at(
        text,
        X0 + 2.0,
        y + 8.0,
        Font::Strong,
        14.0,
        ui.p.text,
        CW,
        false,
    );
    y + 36.0
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn set(cmds: &mut Vec<Cmd>, f: impl FnOnce(&mut Settings) + 'static) {
    cmds.push(Cmd::Set(Box::new(f)));
}

// ----- sidebar ---------------------------------------------------------------------

fn sidebar(ui: &mut Ui, page: &mut usize, logo: Option<&Pixmap>) {
    if let Some(l) = logo {
        let px = (36.0 * ui.s).round() as u32;
        if let Some(small) = crate::icons::scale_to(l, px) {
            ui.image(&small, 22.0, 22.0, 1.0);
        }
    }
    ui.text_at(
        "Berth",
        70.0,
        22.0,
        Font::Strong,
        15.0,
        ui.p.text,
        200.0,
        false,
    );
    ui.text_at(
        "Dock for Windows 11",
        70.0,
        42.0,
        Font::Body,
        12.0,
        ui.p.text2,
        200.0,
        false,
    );
    for (i, (name, glyph)) in PAGES.iter().enumerate() {
        let r = RectF::new(12.0, 92.0 + i as f32 * 40.0, SIDEBAR - 24.0, 36.0);
        if ui.nav_item(idx("nav", i), r, *glyph, name, *page == i) && *page != i {
            *page = i;
            ui.st.want_frame = true;
        }
    }
    let v = format!("Version {}", crate::VERSION);
    ui.text_at(
        &v,
        24.0,
        H - 36.0,
        Font::Body,
        12.0,
        ui.p.text3,
        200.0,
        false,
    );
}

// ----- pages ------------------------------------------------------------------------

fn page_general(ui: &mut Ui, snap: &Snap, cmds: &mut Vec<Cmd>) -> f32 {
    let s = &snap.s;
    let mut y = TOP;
    y = section(ui, y, "Startup");
    let (rows, ny) = card_rows(ui, y, &[ROW]);
    let auto_desc = match snap.auto {
        autostart::State::On => "Berth opens quietly in the background when you sign in",
        autostart::State::Off => "Off: Berth only runs when you open it",
        autostart::State::DisabledByWindows => "Turned off in Windows Settings > Apps > Startup",
    };
    label(ui, rows[0], ROW, "Start with Windows", auto_desc, true);
    if let Some(v) = ui.toggle(
        id("autostart"),
        right(),
        rows[0] + ROW / 2.0,
        snap.auto == autostart::State::On,
        true,
    ) {
        cmds.push(Cmd::Autostart(v));
    }
    y = section(ui, ny + 8.0, "Dock");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Show the dock",
        "Hide it without quitting Berth",
        true,
    );
    if let Some(v) = ui.toggle(
        id("showdock"),
        right(),
        rows[0] + ROW / 2.0,
        !snap.hidden,
        true,
    ) {
        cmds.push(Cmd::ShowDock(v));
    }
    label(
        ui,
        rows[1],
        ROW,
        "Display",
        "The screen that shows the dock",
        true,
    );
    let names: Vec<String> = snap.displays.iter().map(|d| d.0.clone()).collect();
    let sel = snap
        .displays
        .iter()
        .position(|d| d.1.eq_ignore_ascii_case(&s.display))
        .unwrap_or(0);
    if let Some(i) = ui.dropdown(
        id("display"),
        RectF::new(right() - 240.0, rows[1] + 18.0, 240.0, 32.0),
        &names,
        sel,
        true,
    ) {
        if let Some(dev) = snap.displays.get(i).map(|d| d.1.clone()) {
            set(cmds, move |s| s.display = dev);
        }
    }
    label(
        ui,
        rows[2],
        ROW,
        "Position",
        "The screen edge the dock sits on",
        true,
    );
    let pos = match s.position {
        Edge::Bottom => 0,
        Edge::Left => 1,
        Edge::Right => 2,
    };
    if let Some(i) = ui.dropdown(
        id("position"),
        RectF::new(right() - 240.0, rows[2] + 18.0, 240.0, 32.0),
        &strings(&["Bottom", "Left", "Right"]),
        pos,
        true,
    ) {
        let e = [Edge::Bottom, Edge::Left, Edge::Right][i.min(2)];
        set(cmds, move |s| s.position = e);
    }
    y = section(ui, ny + 8.0, "Windows taskbar");
    let (rows, ny) = card_rows(ui, y, &[ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Hide the Windows taskbar",
        "It comes back when you move the pointer to its edge. Restored when you quit Berth.",
        true,
    );
    if let Some(v) = ui.toggle(
        id("taskbar"),
        right(),
        rows[0] + ROW / 2.0,
        s.hide_taskbar,
        true,
    ) {
        set(cmds, move |s| s.hide_taskbar = v);
    }
    y = section(ui, ny + 8.0, "Berth");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Restore default settings",
        "Your pinned apps are kept",
        true,
    );
    if ui.button(
        id("restore"),
        RectF::new(right() - 120.0, rows[0] + 18.0, 120.0, 32.0),
        "Restore",
        false,
        true,
    ) {
        cmds.push(Cmd::Restore);
    }
    let folder = util::data_dir().display().to_string();
    label(ui, rows[1], ROW, "Settings and log folder", &folder, true);
    if ui.button(
        id("folder"),
        RectF::new(right() - 120.0, rows[1] + 18.0, 120.0, 32.0),
        "Open",
        false,
        true,
    ) {
        cmds.push(Cmd::OpenFolder);
    }
    label(
        ui,
        rows[2],
        ROW,
        "Quit Berth",
        "Closes the dock and puts the taskbar back to normal",
        true,
    );
    if ui.button(
        id("quit"),
        RectF::new(right() - 120.0, rows[2] + 18.0, 120.0, 32.0),
        "Quit",
        false,
        true,
    ) {
        cmds.push(Cmd::Quit);
    }
    ny
}

fn slider_row(
    ui: &mut Ui,
    cmds: &mut Vec<Cmd>,
    y: f32,
    key: &str,
    title: &str,
    desc: &str,
    range: (u32, u32),
    value: u32,
    unit: &str,
    enabled: bool,
    apply: fn(&mut Settings, u32),
) {
    label(ui, y, ROW, title, desc, enabled);
    let text = format!("{value}{unit}");
    let r = RectF::new(right() - 220.0, y + ROW / 2.0 - 12.0, 220.0, 24.0);
    if let Some(v) = ui.slider(id(key), r, range.0, range.1, value, &text, enabled) {
        set(cmds, move |s| apply(s, v));
    }
}

fn page_appearance(ui: &mut Ui, snap: &Snap, cmds: &mut Vec<Cmd>) -> f32 {
    let s = &snap.s;
    let mut y = section(ui, TOP, "Look");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Theme",
        "Light, dark, or the same as Windows",
        true,
    );
    let t = match s.theme {
        ThemeMode::System => 0,
        ThemeMode::Light => 1,
        ThemeMode::Dark => 2,
    };
    if let Some(i) = ui.dropdown(
        id("theme"),
        RectF::new(right() - 240.0, rows[0] + 18.0, 240.0, 32.0),
        &strings(&["Match Windows", "Light", "Dark"]),
        t,
        true,
    ) {
        let m = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark][i.min(2)];
        set(cmds, move |s| s.theme = m);
    }
    slider_row(
        ui,
        cmds,
        rows[1],
        "opacity",
        "Background opacity",
        "How see-through the dock is",
        (20, 100),
        s.opacity,
        "%",
        true,
        |s, v| s.opacity = v,
    );
    y = section(ui, ny + 8.0, "Size and spacing");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW, ROW]);
    slider_row(
        ui,
        cmds,
        rows[0],
        "size",
        "Icon size",
        "Size of the icons at rest",
        (24, 128),
        s.icon_size,
        " px",
        true,
        |s, v| s.icon_size = v,
    );
    slider_row(
        ui,
        cmds,
        rows[1],
        "spacing",
        "Space between icons",
        "",
        (0, 32),
        s.spacing,
        " px",
        true,
        |s, v| s.spacing = v,
    );
    slider_row(
        ui,
        cmds,
        rows[2],
        "margin",
        "Distance from screen edge",
        "",
        (0, 64),
        s.edge_margin,
        " px",
        true,
        |s, v| s.edge_margin = v,
    );
    y = section(ui, ny + 8.0, "Hover");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Hover effect",
        "What happens when you point at an icon",
        true,
    );
    let e = match s.hover_effect {
        HoverEffect::Magnify => 0,
        HoverEffect::Lift => 1,
        HoverEffect::Highlight => 2,
    };
    if let Some(i) = ui.dropdown(
        id("effect"),
        RectF::new(right() - 240.0, rows[0] + 18.0, 240.0, 32.0),
        &strings(&["Magnify", "Lift", "Highlight"]),
        e,
        true,
    ) {
        let h = [
            HoverEffect::Magnify,
            HoverEffect::Lift,
            HoverEffect::Highlight,
        ][i.min(2)];
        set(cmds, move |s| s.hover_effect = h);
    }
    let magnify = s.hover_effect == HoverEffect::Magnify;
    slider_row(
        ui,
        cmds,
        rows[1],
        "mag",
        "Magnification",
        "How much icons grow (Magnify only)",
        (110, 200),
        s.magnification,
        "%",
        magnify,
        |s, v| s.magnification = v,
    );
    ny
}

fn page_behavior(ui: &mut Ui, snap: &Snap, cmds: &mut Vec<Cmd>) -> f32 {
    let s = &snap.s;
    let mut y = section(ui, TOP, "Motion");
    let (rows, ny) = card_rows(ui, y, &[ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Animations",
        "Magnify, bounce and slide effects",
        true,
    );
    let a = match s.animations {
        AnimationMode::System => 0,
        AnimationMode::On => 1,
        AnimationMode::Off => 2,
    };
    if let Some(i) = ui.dropdown(
        id("anim"),
        RectF::new(right() - 240.0, rows[0] + 18.0, 240.0, 32.0),
        &strings(&["Match Windows setting", "On", "Off"]),
        a,
        true,
    ) {
        let m = [AnimationMode::System, AnimationMode::On, AnimationMode::Off][i.min(2)];
        set(cmds, move |s| s.animations = m);
    }
    y = section(ui, ny + 8.0, "Running apps");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Show running indicators",
        "A dot under apps that have windows open",
        true,
    );
    if let Some(v) = ui.toggle(
        id("dots"),
        right(),
        rows[0] + ROW / 2.0,
        s.show_running_indicators,
        true,
    ) {
        set(cmds, move |s| s.show_running_indicators = v);
    }
    label(
        ui,
        rows[1],
        ROW,
        "Show apps that aren't pinned",
        "Running apps appear after a divider",
        true,
    );
    if let Some(v) = ui.toggle(
        id("runapps"),
        right(),
        rows[1] + ROW / 2.0,
        s.show_running_apps,
        true,
    ) {
        set(cmds, move |s| s.show_running_apps = v);
    }
    y = section(ui, ny + 8.0, "Visibility");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Automatically hide the dock",
        "Rest the pointer at the screen edge to bring it back",
        true,
    );
    if let Some(v) = ui.toggle(
        id("autohide"),
        right(),
        rows[0] + ROW / 2.0,
        s.auto_hide,
        true,
    ) {
        set(cmds, move |s| s.auto_hide = v);
    }
    label(
        ui,
        rows[1],
        ROW,
        "Hide during full-screen apps",
        "Games, videos and presentations",
        true,
    );
    if let Some(v) = ui.toggle(
        id("fullscreen"),
        right(),
        rows[1] + ROW / 2.0,
        s.hide_in_fullscreen,
        true,
    ) {
        set(cmds, move |s| s.hide_in_fullscreen = v);
    }
    ny
}

fn page_apps(
    ui: &mut Ui,
    snap: &Snap,
    cmds: &mut Vec<Cmd>,
    icons: &mut IconCache,
    pending: &mut bool,
    renaming: Option<usize>,
) -> f32 {
    let items = &snap.s.items;
    let y = TOP + 4.0;
    if ui.button(
        id("addapps"),
        RectF::new(X0, y, 130.0, 32.0),
        "Add apps",
        true,
        true,
    ) {
        cmds.push(Cmd::OpenSheet);
    }
    if ui.button(
        id("addfile"),
        RectF::new(X0 + 140.0, y, 120.0, 32.0),
        "Add file",
        false,
        true,
    ) {
        cmds.push(Cmd::Browse(false));
    }
    if ui.button(
        id("addfolder"),
        RectF::new(X0 + 270.0, y, 120.0, 32.0),
        "Add folder",
        false,
        true,
    ) {
        cmds.push(Cmd::Browse(true));
    }
    let y = y + 48.0;
    if items.is_empty() {
        let (_, ny) = card_rows(ui, y, &[ROW]);
        label(
            ui,
            y,
            ROW,
            "No apps pinned yet",
            "Use Add apps, or drag files and shortcuts onto the dock",
            true,
        );
        return ny;
    }
    let rh = 56.0;
    let heights = vec![rh; items.len()];
    let (rows, ny) = card_rows(ui, y, &heights);
    let px = (24.0 * ui.s).round() as u32;
    let mut loaded = 0;
    for (i, it) in items.iter().enumerate() {
        let ry = rows[i];
        let cy = ry + rh / 2.0;
        // Icon (loaded a few per frame so the page opens instantly).
        let visible = (ry - ui.dy) < H && (ry + rh - ui.dy) > TOP;
        if visible {
            let have = icons.peek(&it.target, px);
            if have.is_none() && loaded < 6 {
                loaded += 1;
                let _ = icons.get(&it.target, px);
            }
            match icons.peek(&it.target, px) {
                Some(Some(pm)) => {
                    let pm = pm.clone();
                    ui.image(&pm, X0 + 18.0, cy - 12.0, 1.0);
                }
                Some(None) => ui.icon('\u{E7C3}', X0 + 30.0, cy, 18.0, ui.p.text2),
                None => *pending = true,
            }
        }
        let tx = X0 + 56.0;
        let maxw = CW - 56.0 - 190.0;
        if renaming == Some(i) {
            ui.st.want_frame = false;
            let r = ui.input_frame(RectF::new(tx - 6.0, cy - 16.0, maxw, 32.0), true);
            RENAME_RECT.with(|c| *c.borrow_mut() = Some(r));
        } else {
            let (_, th) = ui.measure(&it.name, Font::Body, 14.0, maxw, false);
            let desc = describe_target(&it.target);
            let (_, dh) = ui.measure(&desc, Font::Body, 12.0, maxw, false);
            let top = cy - (th + dh - 2.0) / 2.0;
            ui.text_at(&it.name, tx, top, Font::Body, 14.0, ui.p.text, maxw, false);
            ui.text_at(
                &desc,
                tx,
                top + th - 2.0,
                Font::Body,
                12.0,
                ui.p.text2,
                maxw,
                false,
            );
        }
        let bx = right() - 4.0 * 36.0;
        let b = |k: usize| RectF::new(bx + k as f32 * 36.0, cy - 16.0, 32.0, 32.0);
        if ui.icon_button(idx("up", i), b(0), '\u{E70E}', i > 0) {
            cmds.push(Cmd::Move(i, true));
        }
        if ui.icon_button(idx("down", i), b(1), '\u{E70D}', i + 1 < items.len()) {
            cmds.push(Cmd::Move(i, false));
        }
        if ui.icon_button(idx("rename", i), b(2), '\u{E8AC}', true) {
            cmds.push(Cmd::StartRename(i));
        }
        if ui.icon_button(idx("remove", i), b(3), '\u{E74D}', true) {
            cmds.push(Cmd::Remove(i));
        }
    }
    let tip = "Tip: drag files, folders or shortcuts onto the dock to add them, and drag icons along the dock to reorder.";
    let (_, th) = ui.text_at(
        tip,
        X0 + 2.0,
        ny + 6.0,
        Font::Body,
        12.0,
        ui.p.text2,
        CW,
        true,
    );
    ny + 12.0 + th
}

fn describe_target(t: &str) -> String {
    let lower = t.to_ascii_lowercase();
    if lower.starts_with(&apps::APPS_FOLDER_PREFIX.to_ascii_lowercase()) {
        "Installed app".into()
    } else if lower.starts_with("shell:") {
        "Windows location".into()
    } else {
        t.to_string()
    }
}

fn page_updates(ui: &mut Ui, snap: &Snap, cmds: &mut Vec<Cmd>, status: &str, busy: bool) -> f32 {
    let y = TOP + 4.0;
    let hero = 96.0;
    let (rows, ny) = card_rows(ui, y, &[hero]);
    let cy = rows[0] + hero / 2.0;
    ui.circle(X0 + 44.0, cy, 22.0, ui.p.hover);
    ui.icon('\u{E895}', X0 + 44.0, cy, 22.0, ui.p.accent);
    let title = format!("You're on Berth {}", crate::VERSION);
    ui.text_at(
        &title,
        X0 + 82.0,
        cy - 24.0,
        Font::Strong,
        18.0,
        ui.p.text,
        CW - 300.0,
        false,
    );
    let st = if status.is_empty() {
        "Check GitHub for a newer version"
    } else {
        status
    };
    ui.text_at(
        st,
        X0 + 82.0,
        cy + 4.0,
        Font::Body,
        13.0,
        ui.p.text2,
        CW - 310.0,
        true,
    );
    let label_b = if busy {
        "Working\u{2026}"
    } else {
        "Check for updates"
    };
    if ui.button(
        id("check"),
        RectF::new(right() - 170.0, cy - 16.0, 170.0, 32.0),
        label_b,
        true,
        !busy,
    ) {
        cmds.push(Cmd::CheckUpdates);
    }
    let y = section(ui, ny + 8.0, "Notifications");
    let (rows, ny) = card_rows(ui, y, &[ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Notify me when a new version is available",
        "Checks GitHub at start-up and twice a day. Installing always asks first.",
        true,
    );
    if let Some(v) = ui.toggle(
        id("notify"),
        right(),
        rows[0] + ROW / 2.0,
        snap.s.check_updates_on_startup,
        true,
    ) {
        set(cmds, move |s| s.check_updates_on_startup = v);
    }
    let y = section(ui, ny + 8.0, "Release notes");
    let (rows, ny) = card_rows(ui, y, &[ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "What's new in each version",
        "github.com/tahminbinmoin/berth/releases",
        true,
    );
    if ui.button(
        id("releases"),
        RectF::new(right() - 120.0, rows[0] + 18.0, 120.0, 32.0),
        "Open",
        false,
        true,
    ) {
        cmds.push(Cmd::OpenUrl(format!("{}/releases", crate::REPO_URL)));
    }
    let note = "Updates come only from this project's releases on GitHub. Every download is checked against its \
                published SHA-256 checksum before it's installed; Berth then replaces its own program file and \
                restarts. Your settings, pinned apps and startup choice are kept. Nothing about you or your PC is sent.";
    let (_, th) = ui.text_at(
        note,
        X0 + 2.0,
        ny + 6.0,
        Font::Body,
        12.0,
        ui.p.text2,
        CW,
        true,
    );
    ny + 12.0 + th
}

fn page_about(ui: &mut Ui, cmds: &mut Vec<Cmd>, logo: Option<&Pixmap>) -> f32 {
    let y = TOP + 4.0;
    let hero = 112.0;
    let (rows, ny) = card_rows(ui, y, &[hero]);
    if let Some(l) = logo {
        let px = (64.0 * ui.s).round() as u32;
        if let Some(img) = crate::icons::scale_to(l, px) {
            ui.image(&img, X0 + 24.0, rows[0] + 24.0, 1.0);
        }
    }
    ui.text_at(
        "Berth",
        X0 + 108.0,
        rows[0] + 22.0,
        Font::Title,
        22.0,
        ui.p.text,
        400.0,
        false,
    );
    let v = format!(
        "Version {}  \u{00B7}  A lightweight, Mac-style dock for Windows 11",
        crate::VERSION
    );
    ui.text_at(
        &v,
        X0 + 108.0,
        rows[0] + 56.0,
        Font::Body,
        13.0,
        ui.p.text2,
        CW - 130.0,
        false,
    );
    ui.text_at(
        "Free and open source under the MIT License",
        X0 + 108.0,
        rows[0] + 76.0,
        Font::Body,
        13.0,
        ui.p.text2,
        CW - 130.0,
        false,
    );
    let y = section(ui, ny + 8.0, "Links");
    let (rows, ny) = card_rows(ui, y, &[ROW, ROW]);
    label(
        ui,
        rows[0],
        ROW,
        "Project page",
        "Source code, issues and releases on GitHub",
        true,
    );
    if ui.button(
        id("project"),
        RectF::new(right() - 120.0, rows[0] + 18.0, 120.0, 32.0),
        "Open",
        false,
        true,
    ) {
        cmds.push(Cmd::OpenUrl(crate::REPO_URL.to_string()));
    }
    label(
        ui,
        rows[1],
        ROW,
        "Log folder",
        "A small troubleshooting log, kept on this PC only",
        true,
    );
    if ui.button(
        id("logs"),
        RectF::new(right() - 120.0, rows[1] + 18.0, 120.0, 32.0),
        "Open",
        false,
        true,
    ) {
        cmds.push(Cmd::OpenFolder);
    }
    let note = "Privacy: Berth has no accounts, ads, telemetry or cloud features. It only goes online to check this \
                project's GitHub releases for updates. Inspired by the idea of clean desktop docks; not affiliated with \
                Apple, Microsoft or any other dock.";
    let (_, th) = ui.text_at(
        note,
        X0 + 2.0,
        ny + 6.0,
        Font::Body,
        12.0,
        ui.p.text2,
        CW,
        true,
    );
    ny + 12.0 + th
}

// ----- "Add apps" sheet -------------------------------------------------------------

thread_local! {
    static RENAME_RECT: RefCell<Option<RectF>> = const { RefCell::new(None) };
    static SEARCH_RECT: RefCell<Option<RectF>> = const { RefCell::new(None) };
}

fn sheet_ui(
    ui: &mut Ui,
    sheet: &mut Sheet,
    cmds: &mut Vec<Cmd>,
    icons: &mut IconCache,
    pending: &mut bool,
) {
    ui.set_clip(None);
    ui.dy = 0.0;
    let full = RectF::new(0.0, 0.0, W, H);
    ui.fill_round(full, 0.0, ui.p.scrim);
    let (pw, ph) = (560.0, 580.0);
    let panel = RectF::new((W - pw) / 2.0, (H - ph) / 2.0, pw, ph);
    for i in 0..8 {
        let g = 8.0 - i as f32;
        ui.fill_round(
            RectF::new(
                panel.x - g,
                panel.y - g + 4.0,
                panel.w + 2.0 * g,
                panel.h + 2.0 * g,
            ),
            8.0 + g,
            tiny_skia::Color::from_rgba8(0, 0, 0, 8),
        );
    }
    ui.fill_round(panel, 8.0, ui.p.bg);
    ui.stroke_round(panel, 8.0, ui.p.card_border, 1.0);
    let x = panel.x + 24.0;
    let w = panel.w - 48.0;
    ui.text_at(
        "Add apps to the dock",
        x,
        panel.y + 22.0,
        Font::Title,
        20.0,
        ui.p.text,
        w,
        false,
    );
    ui.text_at(
        "Everything in Start > All apps. Select one or more.",
        x,
        panel.y + 54.0,
        Font::Body,
        12.0,
        ui.p.text2,
        w,
        false,
    );
    let sr = ui.input_frame(RectF::new(x, panel.y + 80.0, w, 34.0), true);
    SEARCH_RECT.with(|c| *c.borrow_mut() = Some(sr));
    if sheet.query.is_empty() {
        // Placeholder drawn under the transparent-looking edit.
    }
    // List.
    let list = RectF::new(x, panel.y + 126.0, w, ph - 126.0 - 72.0);
    ui.card(list);
    ui.set_clip(Some(RectF::new(
        list.x + 1.0,
        list.y + 1.0,
        list.w - 2.0,
        list.h - 2.0,
    )));
    let rh = 44.0;
    ui.dy = sheet.scroll;
    let px = (24.0 * ui.s).round() as u32;
    let mut loaded = 0;
    if sheet.shown.is_empty() {
        let msg = if sheet.apps.is_empty() {
            "Couldn't read the list of installed apps."
        } else {
            "No apps match your search."
        };
        ui.text_at(
            msg,
            list.x + 16.0,
            list.y + 16.0 + sheet.scroll,
            Font::Body,
            14.0,
            ui.p.text2,
            list.w - 32.0,
            false,
        );
    }
    for (row, &ai) in sheet.shown.iter().enumerate() {
        let ry = list.y + 4.0 + row as f32 * rh;
        if ry + rh - sheet.scroll < list.y || ry - sheet.scroll > list.y + list.h {
            continue;
        }
        let app = &sheet.apps[ai];
        let r = RectF::new(list.x + 4.0, ry, list.w - 8.0, rh - 2.0);
        let checked = sheet.selected.contains(&app.target);
        let (hov, clicked) = ui.interact(idx("sheet-row", ai), r, false, true);
        if checked {
            ui.fill_round(r, 4.0, ui.p.nav_selected);
        } else if hov {
            ui.fill_round(r, 4.0, ui.p.hover);
        }
        let cy = ry + rh / 2.0 - 1.0;
        ui.checkbox_box(r.x + 10.0, cy, checked);
        if icons.peek(&app.target, px).is_none() && loaded < 8 {
            loaded += 1;
            let _ = icons.get(&app.target, px);
        }
        match icons.peek(&app.target, px) {
            Some(Some(pm)) => {
                let pm = pm.clone();
                ui.image(&pm, r.x + 42.0, cy - 12.0, 1.0);
            }
            Some(None) => ui.icon('\u{E7C3}', r.x + 54.0, cy, 18.0, ui.p.text2),
            None => *pending = true,
        }
        ui.text_mid(
            &app.name,
            r.x + 78.0,
            cy,
            Font::Body,
            14.0,
            ui.p.text,
            r.w - 90.0,
        );
        if clicked {
            if checked {
                sheet.selected.remove(&app.target);
            } else {
                sheet.selected.insert(app.target.clone());
            }
            ui.st.want_frame = true;
        }
    }
    let content = sheet.shown.len() as f32 * rh + 8.0;
    ui.set_clip(None);
    ui.dy = 0.0;
    sheet.scroll = ui.scroll_area(list, content, sheet.scroll);
    // Footer.
    let fy = panel.y + ph - 56.0;
    if ui.button(
        id("sheet-browse"),
        RectF::new(x, fy, 150.0, 32.0),
        "Browse for a file\u{2026}",
        false,
        true,
    ) {
        cmds.push(Cmd::Browse(false));
    }
    let n = sheet.selected.len();
    let add_label = if n == 0 {
        "Add".to_string()
    } else {
        format!("Add ({n})")
    };
    if ui.button(
        id("sheet-add"),
        RectF::new(x + w - 120.0, fy, 120.0, 32.0),
        &add_label,
        true,
        n > 0,
    ) {
        cmds.push(Cmd::SheetAdd);
    }
    if ui.button(
        id("sheet-cancel"),
        RectF::new(x + w - 250.0, fy, 120.0, 32.0),
        "Cancel",
        false,
        true,
    ) {
        cmds.push(Cmd::CloseSheet);
    }
    if ui.input.key == Some(Key::Escape) && ui.st.dropdown.is_none() {
        cmds.push(Cmd::CloseSheet);
    }
}

fn filter_sheet(sheet: &mut Sheet) {
    let q = sheet.query.trim().to_lowercase();
    sheet.shown = (0..sheet.apps.len())
        .filter(|&i| q.is_empty() || sheet.apps[i].name.to_lowercase().contains(&q))
        .collect();
    sheet.scroll = 0.0;
}

// ----- native edit boxes ---------------------------------------------------------------

fn position_edits() {
    let Some((search, rename, sheet_open, renaming)) = with_dash(|d| {
        (
            d.search_edit,
            d.rename_edit,
            d.sheet.is_some(),
            d.rename.is_some(),
        )
    }) else {
        return;
    };
    let place = |h: HWND, r: Option<RectF>, show: bool| unsafe {
        match (r, show) {
            (Some(r), true) => {
                let _ = SetWindowPos(
                    h,
                    None,
                    r.x.round() as i32,
                    r.y.round() as i32,
                    r.w.round() as i32,
                    r.h.round() as i32,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            _ => {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    };
    place(
        search,
        SEARCH_RECT.with(|c| c.borrow_mut().take()),
        sheet_open,
    );
    place(
        rename,
        RENAME_RECT.with(|c| c.borrow_mut().take()),
        renaming,
    );
}

unsafe extern "system" fn edit_proc(
    h: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize || wp.0 == VK_ESCAPE.0 as usize => {
            if let Some(p) = GetParent(h).ok() {
                let commit = wp.0 == VK_RETURN.0 as usize;
                let _ = PostMessageW(
                    Some(p),
                    WM_APP_EDIT_DONE,
                    WPARAM(commit as usize),
                    LPARAM(h.0 as isize),
                );
            }
            LRESULT(0)
        }
        WM_CHAR if wp.0 == 13 || wp.0 == 27 => LRESULT(0),
        WM_KILLFOCUS => {
            if let Some(p) = GetParent(h).ok() {
                let _ = PostMessageW(Some(p), WM_APP_EDIT_DONE, WPARAM(2), LPARAM(h.0 as isize));
            }
            DefSubclassProc(h, msg, wp, lp)
        }
        _ => DefSubclassProc(h, msg, wp, lp),
    }
}

/// Finishes a rename (commit = 1 Enter, 0 Escape, 2 focus lost).
fn edit_done(edit: HWND, how: usize) {
    let Some((rename_edit, idx_)) = with_dash(|d| (d.rename_edit, d.rename)) else {
        return;
    };
    if edit == rename_edit {
        if let Some(i) = idx_ {
            if how != 0 {
                let name = ui::get_text(edit).trim().to_string();
                if !name.is_empty() {
                    run_cmds(vec![Cmd::Set(Box::new(move |s: &mut Settings| {
                        if let Some(it) = s.items.get_mut(i) {
                            it.name = name;
                        }
                    }))]);
                }
            }
            with_dash(|d| d.rename = None);
            if let Some(h) = hwnd() {
                unsafe {
                    let _ = SetFocus(Some(h));
                }
            }
            request_frame();
        }
    } else if how == 0 {
        run_cmds(vec![Cmd::CloseSheet]);
    } else if how == 1 {
        // Enter in the search box adds the selection, if any.
        if with_dash(|d| d.sheet.as_ref().map(|s| !s.selected.is_empty())).flatten() == Some(true) {
            run_cmds(vec![Cmd::SheetAdd]);
        }
    }
}

// ----- commands -----------------------------------------------------------------------

fn run_cmds(cmds: Vec<Cmd>) {
    for c in cmds {
        match c {
            Cmd::Set(f) => {
                with_app(|a| {
                    f(&mut a.settings);
                    a.apply();
                });
            }
            Cmd::Autostart(on) => {
                let owner = hwnd();
                let r = if on {
                    autostart::enable()
                } else {
                    autostart::disable()
                };
                if let Err(e) = r {
                    util::error_box(
                        owner,
                        &format!("Berth couldn't change the launch at sign-in setting.\n\n{e}"),
                    );
                } else if on && autostart::state() == autostart::State::DisabledByWindows {
                    util::info_box(
                        owner,
                        "Berth is registered to start at sign-in, but Windows has it switched off.\n\n\
                         Open Settings > Apps > Startup and switch on \u{201c}Berth\u{201d}.",
                    );
                }
            }
            Cmd::ShowDock(show) => {
                with_app(|a| {
                    a.dock.user_hidden = !show;
                    if show {
                        a.dock.reveal();
                    }
                    a.dock.render(&a.settings);
                });
            }
            Cmd::Restore => crate::restore_defaults(hwnd()),
            Cmd::OpenFolder => crate::perform(Action::OpenDataFolder),
            Cmd::Quit => crate::perform(Action::Quit),
            Cmd::CheckUpdates => crate::check_for_updates(),
            Cmd::OpenUrl(u) => {
                let _ = apps::launch(&u);
            }
            Cmd::OpenSheet => {
                let already = with_dash(|d| d.sheet.is_some()).unwrap_or(true);
                if !already {
                    unsafe {
                        let _ = SetCursor(LoadCursorW(None, IDC_WAIT).ok());
                    }
                    let list = apps::installed_apps();
                    with_dash(|d| {
                        let mut sh = Sheet {
                            apps: list,
                            shown: Vec::new(),
                            selected: HashSet::new(),
                            scroll: 0.0,
                            query: String::new(),
                        };
                        filter_sheet(&mut sh);
                        d.sheet = Some(sh);
                        ui::set_text(d.search_edit, "");
                        ui::send(
                            d.search_edit,
                            ui::c::EM_SETCUEBANNER,
                            1,
                            HSTRING::from("Search apps").as_ptr() as isize,
                        );
                    });
                    frame();
                    if let Some(e) = with_dash(|d| d.search_edit) {
                        unsafe {
                            let _ = SetFocus(Some(e));
                        }
                    }
                }
            }
            Cmd::CloseSheet => {
                with_dash(|d| d.sheet = None);
                if let Some(h) = hwnd() {
                    unsafe {
                        let _ = SetFocus(Some(h));
                    }
                }
                request_frame();
            }
            Cmd::SheetAdd => {
                let items: Vec<PinnedItem> = with_dash(|d| {
                    d.sheet.as_ref().map(|s| {
                        s.apps
                            .iter()
                            .filter(|a| s.selected.contains(&a.target))
                            .map(|a| PinnedItem {
                                name: a.name.clone(),
                                target: a.target.clone(),
                            })
                            .collect()
                    })
                })
                .flatten()
                .unwrap_or_default();
                crate::add_items(items, None);
                run_cmds(vec![Cmd::CloseSheet]);
            }
            Cmd::Browse(folders) => {
                let paths = picker::browse(hwnd(), folders);
                let items: Vec<PinnedItem> = paths.iter().map(|p| apps::item_for_path(p)).collect();
                if !items.is_empty() {
                    crate::add_items(items, None);
                    with_dash(|d| d.sheet = None);
                    request_frame();
                }
            }
            Cmd::StartRename(i) => {
                let name =
                    with_app(|a| a.settings.items.get(i).map(|it| it.name.clone())).flatten();
                if let Some(name) = name {
                    with_dash(|d| {
                        d.rename = Some(i);
                        ui::set_text(d.rename_edit, &name);
                    });
                    frame();
                    if let Some(e) = with_dash(|d| d.rename_edit) {
                        unsafe {
                            let _ = SetFocus(Some(e));
                        }
                        ui::send(e, 0x00B1, 0, -1); // EM_SETSEL: select all
                    }
                }
            }
            Cmd::Move(i, up) => {
                run_cmds(vec![Cmd::Set(Box::new(move |s: &mut Settings| {
                    let j = if up { i.checked_sub(1) } else { Some(i + 1) };
                    if let Some(j) = j {
                        if j < s.items.len() && i < s.items.len() {
                            s.items.swap(i, j);
                        }
                    }
                }))]);
            }
            Cmd::Remove(i) => {
                run_cmds(vec![Cmd::Set(Box::new(move |s: &mut Settings| {
                    if i < s.items.len() {
                        s.items.remove(i);
                    }
                }))]);
            }
        }
    }
}

// ----- window procedure -----------------------------------------------------------------

fn vk_to_key(vk: u16) -> Option<Key> {
    Some(match vk {
        v if v == VK_TAB.0 => Key::Tab,
        v if v == VK_RETURN.0 => Key::Enter,
        v if v == VK_SPACE.0 => Key::Space,
        v if v == VK_LEFT.0 => Key::Left,
        v if v == VK_RIGHT.0 => Key::Right,
        v if v == VK_UP.0 => Key::Up,
        v if v == VK_DOWN.0 => Key::Down,
        v if v == VK_ESCAPE.0 => Key::Escape,
        v if v == VK_HOME.0 => Key::Home,
        v if v == VK_END.0 => Key::End,
        _ => return None,
    })
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let mouse = |d: &mut Dash| {
        d.input.mx = util::get_x_lparam(lp) as f32;
        d.input.my = util::get_y_lparam(lp) as f32;
        d.input.inside = true;
    };
    match msg {
        WM_MOUSEMOVE => {
            let track = with_dash(|d| {
                mouse(d);
                let t = !d.tracking;
                d.tracking = true;
                t
            });
            if track == Some(true) {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            frame();
            LRESULT(0)
        }
        MOUSELEAVE => {
            with_dash(|d| {
                d.tracking = false;
                d.input.inside = false;
            });
            frame();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let _ = SetFocus(Some(hwnd));
            with_dash(|d| {
                mouse(d);
                d.input.down = true;
                d.input.pressed = true;
            });
            frame();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with_dash(|d| {
                mouse(d);
                d.input.down = false;
                d.input.released = true;
            });
            frame();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = util::hiword(wp.0) as i16 as f32 / 120.0;
            with_dash(|d| d.input.wheel += delta);
            frame();
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(k) = vk_to_key(wp.0 as u16) {
                let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                with_dash(|d| {
                    d.input.key = Some(k);
                    d.input.shift = shift;
                });
                frame();
                if k == Key::Escape {
                    let any_open = with_dash(|d| d.st.dropdown.is_some() || d.sheet.is_some())
                        .unwrap_or(false);
                    if !any_open {
                        close();
                    }
                }
            }
            LRESULT(0)
        }
        WM_GETDLGCODE => LRESULT(DLGC_WANTALLKEYS as isize),
        WM_TIMER if wp.0 == TIMER_ICONS => {
            let _ = KillTimer(Some(hwnd), TIMER_ICONS);
            with_dash(|d| d.icons_pending = false);
            frame();
            LRESULT(0)
        }
        WM_APP_FRAME => {
            frame();
            LRESULT(0)
        }
        WM_APP_DASH_REFRESH => {
            with_dash(|d| d.displays = monitors_list());
            frame();
            LRESULT(0)
        }
        WM_APP_EDIT_DONE => {
            edit_done(HWND(lp.0 as *mut _), wp.0);
            LRESULT(0)
        }
        WM_COMMAND => {
            // EN_CHANGE from the search box
            if util::hiword(wp.0) == ui::c::EN_CHANGE && util::loword(wp.0) == 10 {
                let q = with_dash(|d| ui::get_text(d.search_edit)).unwrap_or_default();
                with_dash(|d| {
                    if let Some(s) = d.sheet.as_mut() {
                        s.query = q;
                        filter_sheet(s);
                    }
                });
                frame();
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wp.0 as *mut _);
            let (bg, fg, brush) =
                with_dash(|d| (d.edit_colors.0, d.edit_colors.1, d.edit_brush)).unwrap_or_default();
            SetBkColor(hdc, bg);
            SetTextColor(hdc, fg);
            LRESULT(brush.0 as isize)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with_dash(|d| {
                if let Some(pm) = &d.pm {
                    let (w, h) = (pm.width() as i32, pm.height() as i32);
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
                    SetDIBitsToDevice(
                        hdc,
                        0,
                        0,
                        w as u32,
                        h as u32,
                        0,
                        0,
                        0,
                        h as u32,
                        d.bgra.as_ptr() as *const _,
                        &bmi,
                        DIB_RGB_COLORS,
                    );
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let new_dpi = util::hiword(wp.0) as u32;
            let r = &*(lp.0 as *const RECT);
            with_dash(|d| {
                d.dpi = new_dpi;
                d.text.clear();
                d.icons.clear();
            });
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            update_edit_font();
            frame();
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            with_dash(|d| {
                d.accent = read_accent();
                d.last_dark = None;
            });
            frame();
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_ACTIVATE => {
            request_frame();
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let res = with_dash(|d| (d.edit_font, d.edit_brush));
            if let Some((f, b)) = res {
                ui::delete_font(f);
                if !b.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(b.0));
                }
            }
            DASH.with(|d| {
                if let Ok(mut b) = d.try_borrow_mut() {
                    *b = None;
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
