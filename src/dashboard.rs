//! The Berth Dashboard: one window to control everything — startup,
//! placement, appearance, behavior and the pinned apps. Every change is
//! applied to the dock immediately and saved automatically.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use windows::core::{w, HSTRING, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject, GetSysColor,
    GetSysColorBrush, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, COLOR_WINDOW,
    COLOR_WINDOWTEXT, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HFONT,
    HGDIOBJ, NULL_PEN, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, LVCOLUMNW, LVITEMW, MEASUREITEMSTRUCT, NMHDR, NMLVDISPINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{AnimationMode, Edge, HoverEffect, PinnedItem, Settings, ThemeMode};
use crate::ui::{self, c, child, send};
use crate::{autostart, picker, system, util, with_app, Action};

const CLASS: windows::core::PCWSTR = w!("BerthDashboard");
const WM_APP_DASH_REFRESH: u32 = WM_APP + 40;
const CLIENT_W: i32 = 720;
const CLIENT_H: i32 = 540;
const X0: i32 = 200;
const LABEL_W: i32 = 176;
const CTRL_X: i32 = X0 + 180;
const CTRL_W: i32 = 250;
const VALUE_X: i32 = CTRL_X + CTRL_W + 8;

const PAGES: [&str; 5] = ["General", "Appearance", "Behavior", "Apps", "About"];

// Control IDs.
const NAV: u16 = 100;
const AUTOSTART: u16 = 200;
const AUTOSTART_NOTE: u16 = 201;
const SHOWDOCK: u16 = 202;
const DISPLAY: u16 = 203;
const POSITION: u16 = 204;
const RESTORE: u16 = 205;
const OPENFOLDER: u16 = 206;
const QUIT: u16 = 207;
const BG_NOTE: u16 = 208;
const THEME: u16 = 300;
const SIZE: u16 = 301;
const SIZE_V: u16 = 302;
const SPACING: u16 = 303;
const SPACING_V: u16 = 304;
const MARGIN: u16 = 305;
const MARGIN_V: u16 = 306;
const OPACITY: u16 = 307;
const OPACITY_V: u16 = 308;
const EFFECT: u16 = 309;
const MAG: u16 = 310;
const MAG_V: u16 = 311;
const ANIM: u16 = 400;
const DOTS: u16 = 401;
const RUNAPPS: u16 = 402;
const AUTOHIDE: u16 = 403;
const FULLSCREEN: u16 = 404;
const BEHAVIOR_NOTE: u16 = 405;
const LIST: u16 = 500;
const ADDAPP: u16 = 501;
const ADDFILE: u16 = 502;
const ADDFOLDER: u16 = 503;
const RENAME: u16 = 504;
const UP: u16 = 505;
const DOWN: u16 = 506;
const REMOVE: u16 = 507;
const APPS_NOTE: u16 = 508;
const ABOUT_TEXT: u16 = 600;
const WEBSITE: u16 = 601;
const LOGS: u16 = 602;
const TITLE_BASE: u16 = 900;

const NOTES: [u16; 5] = [
    AUTOSTART_NOTE,
    BG_NOTE,
    BEHAVIOR_NOTE,
    APPS_NOTE,
    ABOUT_TEXT,
];

struct Dash {
    hwnd: HWND,
    dpi: u32,
    font: HFONT,
    bold: HFONT,
    title: HFONT,
    page: usize,
    pages: Vec<Vec<HWND>>,
    all: Vec<HWND>,
    h: HashMap<u16, HWND>,
    displays: Vec<String>,
    items: Vec<PinnedItem>,
    select_after_refresh: Option<usize>,
    nav_brush: HBRUSH,
}

thread_local! {
    static DASH: RefCell<Option<Dash>> = const { RefCell::new(None) };
    static BUILD_DPI: Cell<u32> = const { Cell::new(96) };
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

fn ctl(id: u16) -> HWND {
    with_dash(|d| d.h.get(&id).copied())
        .flatten()
        .unwrap_or_default()
}

pub fn register(hinst: HINSTANCE) -> bool {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc_),
        hInstance: hinst,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        hIcon: crate::tray::app_icon(32),
        hIconSm: crate::tray::app_icon(16),
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        lpszClassName: CLASS,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) != 0 }
}

/// Opens the dashboard (or brings it forward) on a given page.
pub fn open_page(page: usize) {
    if let Some(h) = hwnd() {
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
        show_page(page.min(PAGES.len() - 1));
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
    let scale = dpi as f32 / 96.0;
    let r = ui::centered_window_rect(work, CLIENT_W, CLIENT_H, scale, style, 0, dpi);
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0x10000), // WS_EX_CONTROLPARENT
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
    DASH.with(|d| {
        *d.borrow_mut() = Some(Dash {
            hwnd: h,
            dpi,
            font: HFONT::default(),
            bold: HFONT::default(),
            title: HFONT::default(),
            page: page.min(PAGES.len() - 1),
            pages: Vec::new(),
            all: Vec::new(),
            h: HashMap::new(),
            displays: Vec::new(),
            items: Vec::new(),
            select_after_refresh: None,
            nav_brush: unsafe { CreateSolidBrush(COLORREF(0x00F4_ECE8)) }, // RGB(232,236,244)
        })
    });
    build();
    load_values();
    show_page(page.min(PAGES.len() - 1));
    unsafe {
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
    }
}

pub fn open() {
    open_page(0);
}

pub fn close() {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

/// Asks the dashboard (if open) to re-read settings. Safe to call while the
/// app state is borrowed.
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

pub fn dialog_message(msg: &MSG) -> bool {
    match hwnd() {
        Some(h) => unsafe { IsDialogMessageW(h, msg).as_bool() },
        None => false,
    }
}

/// First launch: open the dashboard and ask about starting with Windows.
pub fn first_run_welcome() {
    open_page(0);
    let owner = hwnd();
    let yes = util::ask_box(
        owner,
        "Welcome to Berth!\n\nStart Berth automatically in the background whenever you sign in to Windows?\n\n\
         You can change this at any time on the General page of this dashboard, or in Windows Settings > Apps > Startup.",
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

// ----- building ------------------------------------------------------------

fn build() {
    let Some((h, dpi)) = with_dash(|d| (d.hwnd, d.dpi)) else {
        return;
    };
    BUILD_DPI.with(|b| b.set(dpi));
    let scale = dpi as f32 / 96.0;
    let font = ui::message_font(dpi, 1.0, 0);
    let bold = ui::message_font(dpi, 1.0, 600);
    let title = ui::message_font(dpi, 1.55, 600);
    let mut map: HashMap<u16, HWND> = HashMap::new();
    let mut pages: Vec<Vec<HWND>> = vec![Vec::new(); PAGES.len()];
    let mut all = Vec::new();

    {
        let mut add = |page: Option<usize>,
                       class: &str,
                       text: &str,
                       style: u32,
                       ex: u32,
                       r: (i32, i32, i32, i32),
                       id: u16,
                       f: HFONT| {
            let c = child(h, class, text, style, ex, r, id, f, scale);
            map.insert(id, c);
            all.push(c);
            if let Some(p) = page {
                pages[p].push(c);
            }
            c
        };

        // Navigation.
        let nav = add(
            None,
            "LISTBOX",
            "",
            c::LBS_NOTIFY | c::LBS_OWNERDRAWFIXED | c::LBS_HASSTRINGS | c::LBS_NOINTEGRALHEIGHT,
            0,
            (12, 16, 172, CLIENT_H - 32),
            NAV,
            font,
        );
        for p in PAGES {
            send(nav, c::LB_ADDSTRING, 0, HSTRING::from(p).as_ptr() as isize);
        }

        for (i, p) in PAGES.iter().enumerate() {
            add(
                Some(i),
                "STATIC",
                p,
                c::SS_NOPREFIX,
                0,
                (X0, 14, 480, 36),
                TITLE_BASE + i as u16,
                title,
            );
        }

        let label = |add: &mut dyn FnMut(
            Option<usize>,
            &str,
            &str,
            u32,
            u32,
            (i32, i32, i32, i32),
            u16,
            HFONT,
        ) -> HWND,
                     page: usize,
                     y: i32,
                     text: &str| {
            add(
                Some(page),
                "STATIC",
                text,
                c::SS_NOPREFIX,
                0,
                (X0, y + 1, LABEL_W, 22),
                0,
                font,
            );
        };
        let combo = |add: &mut dyn FnMut(
            Option<usize>,
            &str,
            &str,
            u32,
            u32,
            (i32, i32, i32, i32),
            u16,
            HFONT,
        ) -> HWND,
                     page: usize,
                     y: i32,
                     id: u16,
                     options: &[&str]| {
            let cb = add(
                Some(page),
                "COMBOBOX",
                "",
                c::CBS_DROPDOWNLIST | WS_VSCROLL.0,
                0,
                (CTRL_X, y, CTRL_W, 300),
                id,
                font,
            );
            for o in options {
                send(cb, c::CB_ADDSTRING, 0, HSTRING::from(*o).as_ptr() as isize);
            }
        };
        let slider = |add: &mut dyn FnMut(
            Option<usize>,
            &str,
            &str,
            u32,
            u32,
            (i32, i32, i32, i32),
            u16,
            HFONT,
        ) -> HWND,
                      page: usize,
                      y: i32,
                      id: u16,
                      vid: u16,
                      min: i32,
                      max: i32| {
            let tb = add(
                Some(page),
                "msctls_trackbar32",
                "",
                c::TBS_NOTICKS,
                0,
                (CTRL_X - 4, y, CTRL_W + 8, 30),
                id,
                font,
            );
            send(tb, c::TBM_SETRANGEMIN, 0, min as isize);
            send(tb, c::TBM_SETRANGEMAX, 0, max as isize);
            send(tb, c::TBM_SETPAGESIZE, 0, 4);
            add(
                Some(page),
                "STATIC",
                "",
                c::SS_NOPREFIX,
                0,
                (VALUE_X, y + 5, 70, 22),
                vid,
                font,
            );
        };
        let check = |add: &mut dyn FnMut(
            Option<usize>,
            &str,
            &str,
            u32,
            u32,
            (i32, i32, i32, i32),
            u16,
            HFONT,
        ) -> HWND,
                     page: usize,
                     y: i32,
                     id: u16,
                     text: &str| {
            add(
                Some(page),
                "BUTTON",
                text,
                c::BS_AUTOCHECKBOX,
                0,
                (X0, y, 490, 26),
                id,
                font,
            );
        };
        let note = |add: &mut dyn FnMut(
            Option<usize>,
            &str,
            &str,
            u32,
            u32,
            (i32, i32, i32, i32),
            u16,
            HFONT,
        ) -> HWND,
                    page: usize,
                    y: i32,
                    hgt: i32,
                    id: u16,
                    text: &str| {
            add(
                Some(page),
                "STATIC",
                text,
                c::SS_NOPREFIX,
                0,
                (X0 + 20, y, 470, hgt),
                id,
                font,
            );
        };
        let button = |add: &mut dyn FnMut(
            Option<usize>,
            &str,
            &str,
            u32,
            u32,
            (i32, i32, i32, i32),
            u16,
            HFONT,
        ) -> HWND,
                      page: usize,
                      r: (i32, i32, i32, i32),
                      id: u16,
                      text: &str| {
            add(Some(page), "BUTTON", text, c::BS_PUSHBUTTON, 0, r, id, font);
        };

        // General.
        check(
            &mut add,
            0,
            66,
            AUTOSTART,
            "Start Berth automatically when I sign in to Windows",
        );
        note(&mut add, 0, 94, 40, AUTOSTART_NOTE, "");
        check(&mut add, 0, 142, SHOWDOCK, "Show the dock");
        label(&mut add, 0, 186, "Display");
        combo(&mut add, 0, 184, DISPLAY, &[]);
        label(&mut add, 0, 228, "Position on screen");
        combo(&mut add, 0, 226, POSITION, &["Bottom", "Left", "Right"]);
        note(
            &mut add,
            0,
            276,
            60,
            BG_NOTE,
            "Berth runs quietly in the background and keeps an icon in the notification area \
             (near the clock). Closing this window does not quit Berth. The Windows taskbar stays \
             available at all times.",
        );
        button(
            &mut add,
            0,
            (X0, 352, 190, 30),
            RESTORE,
            "Restore default settings\u{2026}",
        );
        button(
            &mut add,
            0,
            (X0 + 200, 352, 150, 30),
            OPENFOLDER,
            "Open settings folder",
        );
        button(&mut add, 0, (X0, 392, 190, 30), QUIT, "Quit Berth");

        // Appearance.
        label(&mut add, 1, 68, "Theme");
        combo(&mut add, 1, 66, THEME, &["Match Windows", "Light", "Dark"]);
        label(&mut add, 1, 112, "Icon size");
        slider(&mut add, 1, 110, SIZE, SIZE_V, 24, 128);
        label(&mut add, 1, 156, "Space between icons");
        slider(&mut add, 1, 154, SPACING, SPACING_V, 0, 32);
        label(&mut add, 1, 200, "Distance from screen edge");
        slider(&mut add, 1, 198, MARGIN, MARGIN_V, 0, 64);
        label(&mut add, 1, 244, "Background opacity");
        slider(&mut add, 1, 242, OPACITY, OPACITY_V, 20, 100);
        label(&mut add, 1, 290, "Hover effect");
        combo(&mut add, 1, 288, EFFECT, &["Magnify", "Lift", "Highlight"]);
        label(&mut add, 1, 334, "Magnification");
        slider(&mut add, 1, 332, MAG, MAG_V, 110, 200);

        // Behavior.
        label(&mut add, 2, 68, "Animations");
        combo(
            &mut add,
            2,
            66,
            ANIM,
            &["Match Windows setting", "On", "Off"],
        );
        check(
            &mut add,
            2,
            112,
            DOTS,
            "Show a dot under apps that are running",
        );
        check(
            &mut add,
            2,
            144,
            RUNAPPS,
            "Also show running apps that aren't pinned",
        );
        check(&mut add, 2, 176, AUTOHIDE, "Automatically hide the dock");
        check(
            &mut add,
            2,
            208,
            FULLSCREEN,
            "Hide the dock while an app is full screen",
        );
        note(
            &mut add,
            2,
            244,
            80,
            BEHAVIOR_NOTE,
            "When the dock is hidden automatically, rest the pointer against the screen edge \
             where the dock lives to bring it back. Auto-hide works best on an edge without the \
             taskbar, or with the taskbar set to hide automatically.",
        );

        // Apps.
        let list = add(
            Some(3),
            "SysListView32",
            "",
            c::LVS_REPORT
                | c::LVS_SINGLESEL
                | c::LVS_SHOWSELALWAYS
                | c::LVS_SHAREIMAGELISTS
                | c::LVS_EDITLABELS,
            0x200, // WS_EX_CLIENTEDGE
            (X0, 62, 496, 316),
            LIST,
            font,
        );
        send(
            list,
            c::LVM_SETEXTENDEDLISTVIEWSTYLE,
            c::LVS_EX_FULLROWSELECT | c::LVS_EX_DOUBLEBUFFER,
            (c::LVS_EX_FULLROWSELECT | c::LVS_EX_DOUBLEBUFFER) as isize,
        );
        send(
            list,
            c::LVM_SETIMAGELIST,
            c::LVSIL_SMALL,
            crate::apps::system_image_list(),
        );
        for (i, (name, wdt)) in [("Name", 190), ("Opens", 280)].iter().enumerate() {
            let text = HSTRING::from(*name);
            let col = LVCOLUMNW {
                mask: windows::Win32::UI::Controls::LVCOLUMNW_MASK(c::LVCF_TEXT | c::LVCF_WIDTH),
                cx: (*wdt as f32 * scale) as i32,
                pszText: PWSTR(text.as_ptr() as *mut u16),
                ..Default::default()
            };
            send(list, c::LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
        }
        button(&mut add, 3, (X0, 388, 118, 30), ADDAPP, "Add app\u{2026}");
        button(
            &mut add,
            3,
            (X0 + 126, 388, 118, 30),
            ADDFILE,
            "Add file\u{2026}",
        );
        button(
            &mut add,
            3,
            (X0 + 252, 388, 118, 30),
            ADDFOLDER,
            "Add folder\u{2026}",
        );
        button(&mut add, 3, (X0, 426, 118, 30), RENAME, "Rename");
        button(&mut add, 3, (X0 + 126, 426, 118, 30), UP, "Move up");
        button(&mut add, 3, (X0 + 252, 426, 118, 30), DOWN, "Move down");
        button(&mut add, 3, (X0 + 378, 426, 118, 30), REMOVE, "Remove");
        add(
            Some(3),
            "STATIC",
            "Tip: you can also drag files, folders or shortcuts onto the dock, and drag icons on the dock to reorder them.",
            c::SS_NOPREFIX,
            0,
            (X0, 466, 496, 40),
            APPS_NOTE,
            font,
        );

        // About.
        let about = format!(
            "Berth {}\nA lightweight, Mac-style dock for Windows 11.\n\n\
             Free and open source under the MIT License.\n\n\
             Privacy: Berth has no accounts, ads, telemetry or cloud features and never \
             connects to the internet. Your settings and a small troubleshooting log stay on \
             this PC in:\n{}",
            crate::VERSION,
            util::data_dir().display()
        );
        add(
            Some(4),
            "STATIC",
            &about,
            c::SS_NOPREFIX,
            0,
            (X0, 66, 496, 220),
            ABOUT_TEXT,
            font,
        );
        button(
            &mut add,
            4,
            (X0, 296, 190, 30),
            WEBSITE,
            "Open project page",
        );
        button(
            &mut add,
            4,
            (X0 + 200, 296, 150, 30),
            LOGS,
            "Open log folder",
        );
    }

    with_dash(|d| {
        d.font = font;
        d.bold = bold;
        d.title = title;
        d.h = map;
        d.pages = pages;
        d.all = all;
        d.items.clear();
    });
}

fn destroy_controls() {
    let Some((all, fonts)) = with_dash(|d| {
        let all = std::mem::take(&mut d.all);
        let fonts = [d.font, d.bold, d.title];
        d.h.clear();
        d.pages.clear();
        (all, fonts)
    }) else {
        return;
    };
    for c in all {
        unsafe {
            let _ = DestroyWindow(c);
        }
    }
    for f in fonts {
        ui::delete_font(f);
    }
}

fn show_page(page: usize) {
    let Some((pages, nav)) = with_dash(|d| {
        d.page = page;
        (d.pages.clone(), d.h.get(&NAV).copied().unwrap_or_default())
    }) else {
        return;
    };
    for (i, ctrls) in pages.iter().enumerate() {
        for &c in ctrls {
            unsafe {
                let _ = ShowWindow(c, if i == page { SW_SHOW } else { SW_HIDE });
            }
        }
    }
    send(nav, c::LB_SETCURSEL, page, 0);
}

// ----- values --------------------------------------------------------------

fn set_check(id: u16, on: bool) {
    send(ctl(id), c::BM_SETCHECK, on as usize, 0);
}
fn get_check(id: u16) -> bool {
    send(ctl(id), c::BM_GETCHECK, 0, 0) == 1
}
fn set_combo(id: u16, i: usize) {
    send(ctl(id), c::CB_SETCURSEL, i, 0);
}
fn get_combo(id: u16) -> usize {
    send(ctl(id), c::CB_GETCURSEL, 0, 0).max(0) as usize
}
fn set_slider(id: u16, v: u32) {
    let h = ctl(id);
    if unsafe { GetCapture() } != h {
        send(h, c::TBM_SETPOS, 1, v as isize);
    }
}
fn get_slider(id: u16) -> u32 {
    send(ctl(id), c::TBM_GETPOS, 0, 0).max(0) as u32
}

fn value_labels(s: &Settings) {
    ui::set_text(ctl(SIZE_V), &format!("{} px", s.icon_size));
    ui::set_text(ctl(SPACING_V), &format!("{} px", s.spacing));
    ui::set_text(ctl(MARGIN_V), &format!("{} px", s.edge_margin));
    ui::set_text(ctl(OPACITY_V), &format!("{}%", s.opacity));
    ui::set_text(ctl(MAG_V), &format!("{}%", s.magnification));
    let magnify = s.hover_effect == HoverEffect::Magnify;
    unsafe {
        let _ = EnableWindow(ctl(MAG), magnify);
        let _ = EnableWindow(ctl(MAG_V), magnify);
    }
}

fn load_values() {
    let Some((s, hidden)) = with_app(|a| (a.settings.clone(), a.dock.user_hidden)) else {
        // App busy; try again shortly.
        notify_changed();
        return;
    };

    // General.
    let auto = autostart::state();
    set_check(AUTOSTART, auto == autostart::State::On);
    ui::set_text(
        ctl(AUTOSTART_NOTE),
        match auto {
            autostart::State::On => "Berth will open in the background each time you sign in.",
            autostart::State::Off => "Off. Berth only runs when you open it.",
            autostart::State::DisabledByWindows => {
                "Turned off in Windows Settings > Apps > Startup. Switch \u{201c}Berth\u{201d} on there to use it."
            }
        },
    );
    set_check(SHOWDOCK, !hidden);

    let mons = system::monitors();
    let disp = ctl(DISPLAY);
    send(disp, c::CB_RESETCONTENT, 0, 0);
    let mut devices = vec!["primary".to_string()];
    send(
        disp,
        c::CB_ADDSTRING,
        0,
        HSTRING::from("Main display").as_ptr() as isize,
    );
    for (i, m) in mons.iter().enumerate() {
        send(
            disp,
            c::CB_ADDSTRING,
            0,
            HSTRING::from(m.label(i)).as_ptr() as isize,
        );
        devices.push(m.device.clone());
    }
    let sel = devices
        .iter()
        .position(|d| d.eq_ignore_ascii_case(&s.display))
        .unwrap_or(0);
    set_combo(DISPLAY, sel);
    with_dash(|d| d.displays = devices);
    set_combo(
        POSITION,
        match s.position {
            Edge::Bottom => 0,
            Edge::Left => 1,
            Edge::Right => 2,
        },
    );

    // Appearance.
    set_combo(
        THEME,
        match s.theme {
            ThemeMode::System => 0,
            ThemeMode::Light => 1,
            ThemeMode::Dark => 2,
        },
    );
    set_slider(SIZE, s.icon_size);
    set_slider(SPACING, s.spacing);
    set_slider(MARGIN, s.edge_margin);
    set_slider(OPACITY, s.opacity);
    set_combo(
        EFFECT,
        match s.hover_effect {
            HoverEffect::Magnify => 0,
            HoverEffect::Lift => 1,
            HoverEffect::Highlight => 2,
        },
    );
    set_slider(MAG, s.magnification);
    value_labels(&s);

    // Behavior.
    set_combo(
        ANIM,
        match s.animations {
            AnimationMode::System => 0,
            AnimationMode::On => 1,
            AnimationMode::Off => 2,
        },
    );
    set_check(DOTS, s.show_running_indicators);
    set_check(RUNAPPS, s.show_running_apps);
    set_check(AUTOHIDE, s.auto_hide);
    set_check(FULLSCREEN, s.hide_in_fullscreen);

    // Apps.
    let changed = with_dash(|d| d.items != s.items).unwrap_or(false);
    if changed {
        load_list(&s.items);
    }
    update_app_buttons();
}

fn load_list(items: &[PinnedItem]) {
    let list = ctl(LIST);
    let keep = with_dash(|d| d.select_after_refresh.take())
        .flatten()
        .or_else(|| selected_index());
    send(list, c::LVM_DELETEALLITEMS, 0, 0);
    for (i, it) in items.iter().enumerate() {
        let name = HSTRING::from(it.name.as_str());
        let mut item = LVITEMW {
            mask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_FLAGS(
                c::LVIF_TEXT | c::LVIF_IMAGE | c::LVIF_PARAM,
            ),
            iItem: i as i32,
            pszText: PWSTR(name.as_ptr() as *mut u16),
            iImage: crate::apps::system_icon_index(&it.target),
            lParam: LPARAM(i as isize),
            ..Default::default()
        };
        send(list, c::LVM_INSERTITEMW, 0, &mut item as *mut _ as isize);
        let target = HSTRING::from(describe_target(&it.target));
        let sub = LVITEMW {
            iSubItem: 1,
            pszText: PWSTR(target.as_ptr() as *mut u16),
            ..Default::default()
        };
        send(list, c::LVM_SETITEMTEXTW, i, &sub as *const _ as isize);
    }
    if let Some(k) = keep {
        if !items.is_empty() {
            select_index(k.min(items.len() - 1));
        }
    }
    with_dash(|d| d.items = items.to_vec());
}

fn describe_target(t: &str) -> String {
    let lower = t.to_ascii_lowercase();
    if lower.starts_with(&crate::apps::APPS_FOLDER_PREFIX.to_ascii_lowercase()) {
        "Installed app".into()
    } else if lower.starts_with("shell:") {
        "Windows location".into()
    } else {
        t.to_string()
    }
}

fn selected_index() -> Option<usize> {
    let r = send(
        ctl(LIST),
        c::LVM_GETNEXTITEM,
        usize::MAX,
        c::LVNI_SELECTED as isize,
    );
    (r >= 0).then_some(r as usize)
}

fn select_index(i: usize) {
    let list = ctl(LIST);
    let item = LVITEMW {
        stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            c::LVIS_SELECTED | c::LVIS_FOCUSED,
        ),
        state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            c::LVIS_SELECTED | c::LVIS_FOCUSED,
        ),
        ..Default::default()
    };
    send(list, c::LVM_SETITEMSTATE, i, &item as *const _ as isize);
    send(list, c::LVM_ENSUREVISIBLE, i, 0);
}

fn update_app_buttons() {
    let sel = selected_index();
    let count = with_dash(|d| d.items.len()).unwrap_or(0);
    unsafe {
        let _ = EnableWindow(ctl(RENAME), sel.is_some());
        let _ = EnableWindow(ctl(REMOVE), sel.is_some());
        let _ = EnableWindow(ctl(UP), matches!(sel, Some(i) if i > 0));
        let _ = EnableWindow(ctl(DOWN), matches!(sel, Some(i) if i + 1 < count));
    }
}

/// Applies a settings change to the dock and saves it.
fn change(f: impl FnOnce(&mut Settings)) {
    let s = with_app(|a| {
        f(&mut a.settings);
        a.apply();
        a.settings.clone()
    });
    if let Some(s) = s {
        value_labels(&s);
    }
}

fn on_command(id: u16, code: u16) {
    match (id, code) {
        (NAV, c::LBN_SELCHANGE) => {
            let i = send(ctl(NAV), c::LB_GETCURSEL, 0, 0);
            if i >= 0 {
                show_page(i as usize);
            }
        }
        (AUTOSTART, c::BN_CLICKED) => {
            let want = get_check(AUTOSTART);
            let r = if want {
                autostart::enable()
            } else {
                autostart::disable()
            };
            if let Err(e) = r {
                util::error_box(
                    hwnd(),
                    &format!("Berth couldn't change the launch at sign-in setting.\n\n{e}"),
                );
            } else if want && autostart::state() == autostart::State::DisabledByWindows {
                util::info_box(
                    hwnd(),
                    "Berth is registered to start at sign-in, but Windows has it switched off.\n\n\
                     Open Settings > Apps > Startup and switch on \u{201c}Berth\u{201d}.",
                );
            }
            notify_changed();
        }
        (SHOWDOCK, c::BN_CLICKED) => {
            let show = get_check(SHOWDOCK);
            with_app(|a| {
                a.dock.user_hidden = !show;
                if show {
                    a.dock.reveal();
                }
                a.dock.render(&a.settings);
            });
        }
        (DISPLAY, c::CBN_SELCHANGE) => {
            let i = get_combo(DISPLAY);
            if let Some(dev) = with_dash(|d| d.displays.get(i).cloned()).flatten() {
                change(|s| s.display = dev);
            }
        }
        (POSITION, c::CBN_SELCHANGE) => {
            let e = [Edge::Bottom, Edge::Left, Edge::Right][get_combo(POSITION).min(2)];
            change(|s| s.position = e);
        }
        (THEME, c::CBN_SELCHANGE) => {
            let t = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark][get_combo(THEME).min(2)];
            change(|s| s.theme = t);
        }
        (EFFECT, c::CBN_SELCHANGE) => {
            let e = [
                HoverEffect::Magnify,
                HoverEffect::Lift,
                HoverEffect::Highlight,
            ][get_combo(EFFECT).min(2)];
            change(|s| s.hover_effect = e);
        }
        (ANIM, c::CBN_SELCHANGE) => {
            let m = [AnimationMode::System, AnimationMode::On, AnimationMode::Off]
                [get_combo(ANIM).min(2)];
            change(|s| s.animations = m);
        }
        (DOTS, c::BN_CLICKED) => {
            let v = get_check(DOTS);
            change(|s| s.show_running_indicators = v);
        }
        (RUNAPPS, c::BN_CLICKED) => {
            let v = get_check(RUNAPPS);
            change(|s| s.show_running_apps = v);
        }
        (AUTOHIDE, c::BN_CLICKED) => {
            let v = get_check(AUTOHIDE);
            change(|s| s.auto_hide = v);
        }
        (FULLSCREEN, c::BN_CLICKED) => {
            let v = get_check(FULLSCREEN);
            change(|s| s.hide_in_fullscreen = v);
        }
        (RESTORE, c::BN_CLICKED) => crate::restore_defaults(hwnd()),
        (OPENFOLDER, c::BN_CLICKED) | (LOGS, c::BN_CLICKED) => {
            crate::perform(Action::OpenDataFolder)
        }
        (QUIT, c::BN_CLICKED) => crate::perform(Action::Quit),
        (WEBSITE, c::BN_CLICKED) => {
            let _ = crate::apps::launch(crate::REPO_URL);
        }
        (ADDAPP, c::BN_CLICKED) => picker::open(),
        (ADDFILE, c::BN_CLICKED) | (ADDFOLDER, c::BN_CLICKED) => {
            let paths = picker::browse(hwnd(), id == ADDFOLDER);
            let items: Vec<PinnedItem> = paths
                .iter()
                .map(|p| crate::apps::item_for_path(p))
                .collect();
            if !items.is_empty() {
                crate::add_items(items, None);
            }
        }
        (RENAME, c::BN_CLICKED) => {
            if let Some(i) = selected_index() {
                unsafe {
                    let _ = SetFocus(Some(ctl(LIST)));
                }
                send(ctl(LIST), c::LVM_EDITLABELW, i, 0);
            }
        }
        (UP, c::BN_CLICKED) | (DOWN, c::BN_CLICKED) => {
            if let Some(i) = selected_index() {
                let j = if id == UP {
                    i.checked_sub(1)
                } else {
                    Some(i + 1)
                };
                if let Some(j) = j {
                    with_dash(|d| d.select_after_refresh = Some(j));
                    change(|s| {
                        if j < s.items.len() {
                            s.items.swap(i, j);
                        }
                    });
                }
            }
        }
        (REMOVE, c::BN_CLICKED) => {
            if let Some(i) = selected_index() {
                with_dash(|d| d.select_after_refresh = Some(i));
                change(|s| {
                    if i < s.items.len() {
                        s.items.remove(i);
                    }
                });
            }
        }
        (c::IDCANCEL, _) => close(),
        _ => {}
    }
}

fn on_hscroll(source: HWND) {
    let id = [SIZE, SPACING, MARGIN, OPACITY, MAG]
        .into_iter()
        .find(|&i| ctl(i) == source);
    let Some(id) = id else { return };
    let v = get_slider(id);
    change(|s| match id {
        SIZE => s.icon_size = v,
        SPACING => s.spacing = v,
        MARGIN => s.edge_margin = v,
        OPACITY => s.opacity = v,
        MAG => s.magnification = v,
        _ => {}
    });
}

unsafe fn on_notify(hdr: &NMHDR, lp: LPARAM) -> LRESULT {
    if hdr.idFrom != LIST as usize {
        return LRESULT(0);
    }
    match hdr.code as i32 {
        c::LVN_ITEMCHANGED => update_app_buttons(),
        c::NM_DBLCLK => on_command(RENAME, c::BN_CLICKED),
        c::LVN_BEGINLABELEDITW => return LRESULT(0),
        c::LVN_ENDLABELEDITW => {
            let info = &*(lp.0 as *const NMLVDISPINFOW);
            if !info.item.pszText.is_null() {
                let name = info
                    .item
                    .pszText
                    .to_string()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let idx = info.item.iItem;
                if !name.is_empty() && idx >= 0 {
                    with_dash(|d| d.select_after_refresh = Some(idx as usize));
                    change(|s| {
                        if let Some(it) = s.items.get_mut(idx as usize) {
                            it.name = name;
                        }
                    });
                    return LRESULT(1);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    LRESULT(0)
}

unsafe fn draw_nav_item(di: &DRAWITEMSTRUCT) {
    let Some((font, bold, brush, dpi)) = with_dash(|d| (d.font, d.bold, d.nav_brush, d.dpi)) else {
        return;
    };
    let scale = dpi as f32 / 96.0;
    let hdc: HDC = di.hDC;
    let rc = di.rcItem;
    FillRect(hdc, &rc, GetSysColorBrush(COLOR_WINDOW));
    if di.itemID == u32::MAX {
        return;
    }
    let selected = di.itemState.0 & 1 != 0; // ODS_SELECTED
    if selected {
        let inset = (2.0 * scale) as i32;
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let r = (8.0 * scale) as i32;
        let _ = RoundRect(
            hdc,
            rc.left + inset,
            rc.top + inset,
            rc.right - inset,
            rc.bottom - inset,
            r,
            r,
        );
        // Accent bar.
        let accent = CreateSolidBrush(COLORREF(0x00C0_6700)); // RGB(0,103,192)
        let bar_h = (16.0 * scale) as i32;
        let cy = (rc.top + rc.bottom) / 2;
        let bar = RECT {
            left: rc.left + inset,
            top: cy - bar_h / 2,
            right: rc.left + inset + (3.0 * scale) as i32,
            bottom: cy + bar_h / 2,
        };
        FillRect(hdc, &bar, accent);
        let _ = DeleteObject(HGDIOBJ(accent.0));
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
    let old_font = SelectObject(hdc, HGDIOBJ(if selected { bold.0 } else { font.0 }));
    let mut text: Vec<u16> = PAGES
        .get(di.itemID as usize)
        .unwrap_or(&"")
        .encode_utf16()
        .collect();
    let mut tr = RECT {
        left: rc.left + (16.0 * scale) as i32,
        ..rc
    };
    DrawTextW(
        hdc,
        &mut text,
        &mut tr,
        DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
    SelectObject(hdc, old_font);
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            on_command(util::loword(wp.0), util::hiword(wp.0));
            LRESULT(0)
        }
        WM_HSCROLL => {
            on_hscroll(HWND(lp.0 as *mut _));
            LRESULT(0)
        }
        WM_NOTIFY => on_notify(&*(lp.0 as *const NMHDR), lp),
        WM_MEASUREITEM => {
            let mi = &mut *(lp.0 as *mut MEASUREITEMSTRUCT);
            let dpi = BUILD_DPI.with(|b| b.get());
            mi.itemHeight = (38.0 * dpi as f32 / 96.0) as u32;
            LRESULT(1)
        }
        WM_DRAWITEM => {
            let di = &*(lp.0 as *const DRAWITEMSTRUCT);
            if di.CtlID == NAV as u32 {
                draw_nav_item(di);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = HDC(wp.0 as *mut _);
            let control = HWND(lp.0 as *mut _);
            let id = GetDlgCtrlID(control) as u16;
            SetBkColor(hdc, COLORREF(GetSysColor(COLOR_WINDOW)));
            if NOTES.contains(&id) {
                SetTextColor(hdc, COLORREF(0x0060_6060));
            }
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_APP_DASH_REFRESH => {
            load_values();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let new_dpi = util::hiword(wp.0) as u32;
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let page = with_dash(|d| {
                d.dpi = new_dpi;
                d.page
            })
            .unwrap_or(0);
            destroy_controls();
            build();
            load_values();
            show_page(page);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            destroy_controls();
            let brush = with_dash(|d| d.nav_brush);
            if let Some(b) = brush {
                let _ = DeleteObject(HGDIOBJ(b.0));
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
