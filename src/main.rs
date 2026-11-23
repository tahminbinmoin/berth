//! Berth — a lightweight, Mac-style dock for Windows 11.
//!
//! Architecture (single UI thread):
//! - `App` holds settings and the dock; it lives in a thread-local and is
//!   only borrowed for short, non-reentrant sections. Anything that can run
//!   a nested message loop (menus, message boxes, ShellExecute, dialogs)
//!   happens outside the borrow.
//! - A hidden "controller" window owns the tray icon, menus and timers.
//! - The dock itself is a layered window (see `dock.rs`).

#![windows_subsystem = "windows"]
#![allow(clippy::missing_safety_doc)]

mod apps;
mod autostart;
mod config;
mod dashboard;
mod dock;
mod draw;
mod icons;
mod picker;
mod system;
mod taskbar;
mod text;
mod tray;
mod ui;
mod updater;
mod util;

use std::cell::RefCell;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{
    CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{PinnedItem, Settings};
use dock::Dock;
use util::{error_box, from_wide, get_x_lparam, get_y_lparam, info_box, log};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO_URL: &str = "https://github.com/tahminbinmoin/berth";

const CONTROLLER_CLASS: PCWSTR = w!("BerthController");
const DOCK_CLASS: PCWSTR = w!("BerthDock");

pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_SHOW_SETTINGS: u32 = WM_APP + 2;
pub const WM_APP_RELAYOUT: u32 = WM_APP + 3;
pub const WM_APP_ACTION: u32 = WM_APP + 4;
pub const WM_APP_UPDATE: u32 = WM_APP + 5;

const TIMER_SAVE: usize = 10;
const TIMER_RELAYOUT: usize = 11;
const TIMER_UPDATE_CHECK: usize = 12;
/// How often Berth looks for updates while running (if notifications are on).
const UPDATE_CHECK_INTERVAL_MS: u32 = 12 * 60 * 60 * 1000;

/// What's new in this version, shown after an update.
const WHATS_NEW: &str = "New: Berth can hide the Windows taskbar for you. It slides back in when you move the pointer to its edge (Dashboard > General).";

pub struct App {
    pub settings: Settings,
    pub controller: HWND,
    pub dock: Dock,
    pub tray: tray::Tray,
    save_pending: bool,
    /// Self-test runs never write settings.
    pub persist: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Returns `None` if the state is already
/// borrowed further up the stack (re-entrancy from a nested message loop).
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut b = cell.try_borrow_mut().ok()?;
        let app = b.as_mut()?;
        Some(f(app))
    })
}

impl App {
    /// Applies changed settings to the dock immediately and saves soon.
    pub fn apply(&mut self) {
        self.settings.sanitize();
        self.dock.sync_items(&self.settings);
        self.dock.relayout(&self.settings);
        self.dock.render(&self.settings);
        self.sync_taskbar();
        self.schedule_save();
        dashboard::notify_changed();
    }

    /// Applies the "hide the Windows taskbar" option: turns Windows' own
    /// taskbar auto-hide on (remembering the previous state) or restores it.
    pub fn sync_taskbar(&mut self) {
        if !self.persist {
            return; // self-test / preview runs never touch the taskbar
        }
        if self.settings.hide_taskbar {
            if self.settings.taskbar_autohide_before.is_none() {
                match taskbar::is_autohide() {
                    Some(prev) => {
                        self.settings.taskbar_autohide_before = Some(prev);
                        self.schedule_save();
                    }
                    None => return, // no taskbar running right now
                }
            }
            taskbar::set_autohide(true);
        } else if let Some(prev) = self.settings.taskbar_autohide_before {
            if taskbar::set_autohide(prev) {
                self.settings.taskbar_autohide_before = None;
                self.schedule_save();
            }
        }
    }

    /// Puts the taskbar back the way it was (on quit or sign-out). The
    /// option itself stays on, so it is applied again next time.
    pub fn restore_taskbar(&mut self) {
        if let Some(prev) = self.settings.taskbar_autohide_before {
            if taskbar::set_autohide(prev) {
                self.settings.taskbar_autohide_before = None;
                self.save_pending = self.persist;
            }
        }
    }

    pub fn schedule_save(&mut self) {
        if !self.persist {
            return;
        }
        self.save_pending = true;
        unsafe { SetTimer(Some(self.controller), TIMER_SAVE, 600, None) };
    }

    pub fn save_now(&mut self) -> Result<(), String> {
        unsafe {
            let _ = KillTimer(Some(self.controller), TIMER_SAVE);
        }
        if !self.persist || !self.save_pending {
            return Ok(());
        }
        self.save_pending = false;
        config::save(&self.settings).inspect_err(|e| log(&format!("save failed: {e}")))
    }
}

/// Things the user can do from menus, the settings window or the dock.
#[derive(Clone, Debug)]
pub enum Action {
    Open { idx: usize, new_instance: bool },
    Remove(usize),
    KeepInDock(usize),
    OpenLocation(usize),
    AddApp,
    Settings,
    ToggleDock,
    ToggleAutostart,
    RestoreDefaults,
    OpenDataFolder,
    CheckUpdates,
    About,
    Quit,
}

// ----- menus ---------------------------------------------------------------

const ID_OPEN: u32 = 1;
const ID_NEW: u32 = 2;
const ID_REMOVE: u32 = 3;
const ID_KEEP: u32 = 4;
const ID_LOCATION: u32 = 5;
const ID_ADD: u32 = 10;
const ID_SETTINGS: u32 = 11;
const ID_TOGGLE: u32 = 12;
const ID_AUTOSTART: u32 = 13;
const ID_RESTORE: u32 = 14;
const ID_DATAFOLDER: u32 = 15;
const ID_ABOUT: u32 = 16;
const ID_QUIT: u32 = 17;
const ID_UPDATE: u32 = 18;

unsafe fn append(menu: HMENU, id: u32, text: &str, flags: MENU_ITEM_FLAGS) {
    let _ = AppendMenuW(menu, MF_STRING | flags, id as usize, &HSTRING::from(text));
}
unsafe fn separator(menu: HMENU) {
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
}

/// Shows a popup menu and returns the chosen command (0 = dismissed).
fn track_menu(menu: HMENU, x: i32, y: i32) -> u32 {
    let Some(owner) = with_app(|a| {
        a.dock.menu_open = true;
        a.controller
    }) else {
        return 0;
    };
    let cmd = unsafe {
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0,
            x,
            y,
            owner,
            None,
        );
        if r.0 == 0 {
            let err = windows::Win32::Foundation::GetLastError();
            if err.0 != 0 {
                log(&format!("menu could not be shown: error {}", err.0));
            }
        }
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        r.0 as u32
    };
    with_app(|a| {
        a.dock.menu_open = false;
        a.dock.ensure_frame_timer();
    });
    cmd
}

fn show_dock_menu(idx: Option<usize>, x: i32, y: i32) {
    let info = with_app(|a| {
        idx.and_then(|i| a.dock.entries.get(i)).map(|e| {
            (
                e.pinned,
                e.running(),
                !e.item.target.to_ascii_lowercase().starts_with("shell:"),
                e.keys.exists,
            )
        })
    })
    .flatten();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        if let (Some(_), Some((pinned, running, is_file, exists))) = (idx, info) {
            if running {
                append(menu, ID_OPEN, "Show", MENU_ITEM_FLAGS(0));
                append(menu, ID_NEW, "Open new window", MENU_ITEM_FLAGS(0));
            } else {
                append(menu, ID_OPEN, "Open", MENU_ITEM_FLAGS(0));
            }
            if is_file {
                append(
                    menu,
                    ID_LOCATION,
                    "Open file location",
                    if exists {
                        MENU_ITEM_FLAGS(0)
                    } else {
                        MF_GRAYED
                    },
                );
            }
            separator(menu);
            if pinned {
                append(menu, ID_REMOVE, "Remove from Dock", MENU_ITEM_FLAGS(0));
            } else {
                append(menu, ID_KEEP, "Keep in Dock", MENU_ITEM_FLAGS(0));
            }
            separator(menu);
        }
        append(menu, ID_ADD, "Add app\u{2026}", MENU_ITEM_FLAGS(0));
        append(
            menu,
            ID_SETTINGS,
            "Open Dashboard\u{2026}",
            MENU_ITEM_FLAGS(0),
        );
        if idx.is_none() {
            append(menu, ID_TOGGLE, "Hide dock", MENU_ITEM_FLAGS(0));
            separator(menu);
            append(menu, ID_QUIT, "Quit Berth", MENU_ITEM_FLAGS(0));
        }
        let cmd = track_menu(menu, x, y);
        let action = match (cmd, idx) {
            (ID_OPEN, Some(i)) => Some(Action::Open {
                idx: i,
                new_instance: false,
            }),
            (ID_NEW, Some(i)) => Some(Action::Open {
                idx: i,
                new_instance: true,
            }),
            (ID_REMOVE, Some(i)) => Some(Action::Remove(i)),
            (ID_KEEP, Some(i)) => Some(Action::KeepInDock(i)),
            (ID_LOCATION, Some(i)) => Some(Action::OpenLocation(i)),
            (ID_ADD, _) => Some(Action::AddApp),
            (ID_SETTINGS, _) => Some(Action::Settings),
            (ID_TOGGLE, _) => Some(Action::ToggleDock),
            (ID_QUIT, _) => Some(Action::Quit),
            _ => None,
        };
        if let Some(a) = action {
            perform(a);
        }
    }
}

fn show_tray_menu(x: i32, y: i32) {
    let Some(hidden) = with_app(|a| a.dock.user_hidden) else {
        return;
    };
    let auto = autostart::state();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        append(
            menu,
            ID_TOGGLE,
            "Show dock",
            if hidden { MF_UNCHECKED } else { MF_CHECKED },
        );
        append(menu, ID_ADD, "Add app\u{2026}", MENU_ITEM_FLAGS(0));
        append(
            menu,
            ID_SETTINGS,
            "Open Dashboard\u{2026}",
            MENU_ITEM_FLAGS(0),
        );
        separator(menu);
        let label = match auto {
            autostart::State::DisabledByWindows => {
                "Launch at sign-in (off in Windows Startup settings)"
            }
            _ => "Launch at sign-in",
        };
        append(
            menu,
            ID_AUTOSTART,
            label,
            if auto == autostart::State::On {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            },
        );
        if let Ok(sub) = CreatePopupMenu() {
            append(
                sub,
                ID_RESTORE,
                "Restore default settings\u{2026}",
                MENU_ITEM_FLAGS(0),
            );
            append(
                sub,
                ID_DATAFOLDER,
                "Open settings folder",
                MENU_ITEM_FLAGS(0),
            );
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Troubleshooting"));
        }
        separator(menu);
        append(
            menu,
            ID_UPDATE,
            "Check for updates\u{2026}",
            MENU_ITEM_FLAGS(0),
        );
        append(menu, ID_ABOUT, "About Berth", MENU_ITEM_FLAGS(0));
        append(menu, ID_QUIT, "Quit Berth", MENU_ITEM_FLAGS(0));
        let cmd = track_menu(menu, x, y);
        let action = match cmd {
            ID_TOGGLE => Some(Action::ToggleDock),
            ID_ADD => Some(Action::AddApp),
            ID_SETTINGS => Some(Action::Settings),
            ID_AUTOSTART => Some(Action::ToggleAutostart),
            ID_RESTORE => Some(Action::RestoreDefaults),
            ID_DATAFOLDER => Some(Action::OpenDataFolder),
            ID_UPDATE => Some(Action::CheckUpdates),
            ID_ABOUT => Some(Action::About),
            ID_QUIT => Some(Action::Quit),
            _ => None,
        };
        if let Some(a) = action {
            perform(a);
        }
    }
}

// ----- actions -------------------------------------------------------------

fn controller() -> Option<HWND> {
    with_app(|a| a.controller)
}

pub fn perform(action: Action) {
    match action {
        Action::Open { idx, new_instance } => open_entry(idx, new_instance),
        Action::Remove(idx) => {
            with_app(|a| {
                if let Some(e) = a.dock.entries.get(idx) {
                    if e.pinned {
                        let t = e.item.target.clone();
                        a.settings
                            .items
                            .retain(|i| !i.target.eq_ignore_ascii_case(&t));
                        a.apply();
                    }
                }
            });
        }
        Action::KeepInDock(idx) => {
            with_app(|a| {
                if let Some(e) = a.dock.entries.get(idx) {
                    if !e.pinned && a.settings.items.len() < config::MAX_ITEMS {
                        a.settings.items.push(e.item.clone());
                        a.apply();
                    }
                }
            });
        }
        Action::OpenLocation(idx) => {
            if let Some(t) =
                with_app(|a| a.dock.entries.get(idx).map(|e| e.item.target.clone())).flatten()
            {
                apps::reveal_in_explorer(&t);
            }
        }
        Action::AddApp => picker::open(),
        Action::Settings => dashboard::open(),
        Action::ToggleDock => {
            with_app(|a| {
                a.dock.user_hidden = !a.dock.user_hidden;
                if !a.dock.user_hidden {
                    a.dock.reveal();
                }
                a.dock.render(&a.settings);
            });
            dashboard::notify_changed();
        }
        Action::ToggleAutostart => toggle_autostart(controller()),
        Action::RestoreDefaults => restore_defaults(controller()),
        Action::OpenDataFolder => {
            let dir = util::data_dir();
            let _ = apps::launch(&dir.to_string_lossy());
        }
        Action::CheckUpdates => check_for_updates(),
        Action::About => info_box(
            None,
            &format!(
                "Berth {VERSION}\nA lightweight dock for Windows 11.\n\n\
                 Free and open source under the MIT License.\n{REPO_URL}\n\n\
                 Berth only goes online to check GitHub for updates. Settings and the log are stored in:\n{}",
                util::data_dir().display()
            ),
        ),
        Action::Quit => quit(),
    }
}

fn open_entry(idx: usize, new_instance: bool) {
    let Some(Some((target, name, windows, pinned))) = with_app(|a| {
        a.dock.entries.get(idx).map(|e| {
            (
                e.item.target.clone(),
                e.item.name.clone(),
                e.windows.clone(),
                e.pinned,
            )
        })
    }) else {
        return;
    };
    if !new_instance && !windows.is_empty() {
        apps::activate(&windows);
        return;
    }
    with_app(|a| a.dock.start_bounce(idx));
    match apps::launch(&target) {
        Ok(()) => {}
        Err(apps::LaunchError::Cancelled) => {}
        Err(apps::LaunchError::Missing) => {
            log(&format!("target missing: {target}"));
            let msg = format!(
                "\u{201c}{name}\u{201d} couldn't be opened because it was moved or uninstalled.\n\n{target}\n\nRemove it from the dock?"
            );
            if pinned && util::yes_no_box(None, &msg) {
                with_app(|a| {
                    a.settings
                        .items
                        .retain(|i| !i.target.eq_ignore_ascii_case(&target));
                    a.apply();
                });
            } else if !pinned {
                error_box(None, &format!("\u{201c}{name}\u{201d} couldn't be opened."));
            }
        }
        Err(apps::LaunchError::Other(msg)) => {
            log(&format!("launch failed for {target}: {msg}"));
            error_box(
                None,
                &format!("\u{201c}{name}\u{201d} couldn't be opened.\n\nWindows said: {msg}"),
            );
        }
    }
}

pub fn toggle_autostart(owner: Option<HWND>) {
    let result = match autostart::state() {
        autostart::State::On => autostart::disable(),
        autostart::State::Off => autostart::enable(),
        autostart::State::DisabledByWindows => {
            info_box(
                owner,
                "Launch at sign-in is registered, but it is turned off in Windows.\n\n\
                 To turn it on, open Settings > Apps > Startup and switch on \u{201c}Berth\u{201d}.\n\
                 To remove it completely, choose this menu item again after turning it on, or \
                 switch it off there.",
            );
            return;
        }
    };
    if let Err(e) = result {
        error_box(
            owner,
            &format!("Berth couldn't change the launch at sign-in setting.\n\n{e}"),
        );
    }
    dashboard::refresh();
}

pub fn restore_defaults(owner: Option<HWND>) {
    if !util::confirm_box(
        owner,
        "Restore Berth's default settings?\n\nPosition, size, appearance and behavior go back to their defaults. \
         Your pinned apps are kept. Launch at sign-in is not changed.",
    ) {
        return;
    }
    with_app(|a| {
        a.settings = a.settings.defaults_keeping_items();
        a.dock.user_hidden = false;
        a.apply();
    });
    dashboard::refresh();
}

/// "Check for updates" from the dashboard or tray: check, then offer to
/// download and install.
pub fn check_for_updates() {
    dashboard::open_page(dashboard::UPDATES_PAGE);
    let Some(ctl) = controller() else { return };
    if updater::start_check(ctl, WM_APP_UPDATE, false) {
        dashboard::set_update_status("Checking for updates\u{2026}", true);
    }
}

fn handle_update_outcome() {
    let Some(outcome) = updater::take_outcome() else {
        return;
    };
    let owner = dashboard::window();
    match outcome {
        updater::Outcome::Checked { silent, result } => match result {
            Err(e) => {
                if !silent {
                    dashboard::set_update_status(
                        &format!("Couldn't check for updates: {e}."),
                        false,
                    );
                }
            }
            Ok(None) => {
                if !silent {
                    dashboard::set_update_status(
                        &format!("You're up to date. Berth {VERSION} is the latest version."),
                        false,
                    );
                }
            }
            Ok(Some(r)) => {
                dashboard::set_update_status(&format!("Berth {} is available.", r.version), false);
                if silent {
                    // Don't repeat the same notification every few hours.
                    let already = LAST_NOTIFIED.with(|l| {
                        let mut l = l.borrow_mut();
                        let seen = *l == r.version;
                        *l = r.version.clone();
                        seen
                    });
                    if already {
                        return;
                    }
                    with_app(|a| {
                        a.tray.notify(
                            &format!("Berth {} is available", r.version),
                            "Click here, then choose \u{201c}Check for updates and install\u{201d}.",
                        )
                    });
                    return;
                }
                let ok = util::ask_box(
                    owner,
                    &format!(
                        "Berth {} is available (you have {VERSION}).\n\nDownload and install it now? Berth will \
                         restart by itself. Your settings and pinned apps are kept.",
                        r.version
                    ),
                );
                if ok {
                    if let Some(ctl) = controller() {
                        if updater::start_download(r.clone(), ctl, WM_APP_UPDATE) {
                            dashboard::set_update_status(
                                &format!("Downloading Berth {}\u{2026}", r.version),
                                true,
                            );
                        }
                    }
                }
            }
        },
        updater::Outcome::Prepared(Err(e)) => {
            dashboard::set_update_status(&format!("The update couldn't be downloaded: {e}"), false);
            error_box(
                owner,
                &format!("Berth couldn't download the update.\n\n{e}"),
            );
        }
        updater::Outcome::Prepared(Ok((release, new_exe))) => {
            dashboard::set_update_status(
                &format!("Installing Berth {}\u{2026}", release.version),
                true,
            );
            match updater::install(&new_exe) {
                Err(e) => {
                    dashboard::set_update_status("The update couldn't be installed.", false);
                    error_box(owner, &format!("Berth couldn't install the update.\n\n{e}"));
                }
                Ok(exe) => {
                    with_app(|a| {
                        a.save_pending = a.persist;
                        let _ = a.save_now();
                    });
                    // Let the new version start even before this one has exited.
                    release_single_instance();
                    let started = std::process::Command::new(&exe)
                        .arg("--after-update")
                        .arg(std::process::id().to_string())
                        .spawn();
                    match started {
                        Ok(_) => {
                            log(&format!("updated to {}; restarting", release.version));
                            quit();
                        }
                        Err(e) => {
                            updater::rollback(&exe);
                            dashboard::set_update_status(
                                "The update couldn't be started, so it was undone.",
                                false,
                            );
                            error_box(
                                owner,
                                &format!("The new version couldn't be started ({e}). Berth was left unchanged."),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn quit() {
    with_app(|a| {
        a.restore_taskbar();
        let _ = a.save_now();
        a.tray.remove();
    });
    dashboard::close();
    picker::close();
    if let Some((dock, ctl)) = with_app(|a| (a.dock.hwnd, a.controller)) {
        unsafe {
            let _ = DestroyWindow(dock);
            let _ = DestroyWindow(ctl);
        }
    }
}

/// Adds items (from the picker or drag and drop) at `index` among pinned items.
pub fn add_items(items: Vec<PinnedItem>, index: Option<usize>) -> usize {
    with_app(|a| {
        let mut added = 0;
        let mut at = index
            .unwrap_or(a.settings.items.len())
            .min(a.settings.items.len());
        for it in items {
            if a.settings.items.len() >= config::MAX_ITEMS {
                break;
            }
            if a.settings
                .items
                .iter()
                .any(|x| x.target.eq_ignore_ascii_case(&it.target))
            {
                continue;
            }
            a.settings.items.insert(at, it);
            at += 1;
            added += 1;
        }
        if added > 0 {
            a.apply();
        }
        added
    })
    .unwrap_or(0)
}

// ----- window procedures ---------------------------------------------------

static mut TASKBAR_CREATED: u32 = 0;

thread_local! {
    static LAST_NOTIFIED: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The single-instance mutex, released early when restarting after an update.
static SINGLE_INSTANCE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn release_single_instance() {
    let h = SINGLE_INSTANCE.swap(0, std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let m = windows::Win32::Foundation::HANDLE(h as *mut _);
            let _ = windows::Win32::System::Threading::ReleaseMutex(m);
            let _ = windows::Win32::Foundation::CloseHandle(m);
        }
    }
}

unsafe extern "system" fn controller_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_TRAY => {
            let event = (lp.0 as u32) & 0xffff;
            match event {
                WM_CONTEXTMENU => show_tray_menu(util::get_x_wparam(wp), util::get_y_wparam(wp)),
                // NIN_SELECT / NIN_KEYSELECT (left click or Enter on the icon)
                0x0400 | 0x0401 => dashboard::open(),
                // NIN_BALLOONUSERCLICK: the update notification was clicked
                0x0405 => dashboard::open_page(dashboard::UPDATES_PAGE),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_SHOW_SETTINGS => {
            with_app(|a| {
                a.dock.user_hidden = false;
                a.dock.reveal();
            });
            dashboard::open();
            LRESULT(0)
        }
        WM_APP_UPDATE => {
            handle_update_outcome();
            LRESULT(0)
        }
        WM_APP_RELAYOUT => {
            let _ = SetTimer(Some(hwnd), TIMER_RELAYOUT, 250, None);
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_SAVE => {
                    if let Some(Err(e)) = with_app(|a| a.save_now()) {
                        error_box(None, &format!("Berth couldn't save its settings.\n\n{e}"));
                    }
                }
                TIMER_UPDATE_CHECK => {
                    if with_app(|a| a.settings.check_updates_on_startup) == Some(true) {
                        updater::start_check(hwnd, WM_APP_UPDATE, true);
                    }
                }
                TIMER_RELAYOUT => {
                    let _ = KillTimer(Some(hwnd), TIMER_RELAYOUT);
                    if with_app(|a| {
                        a.dock.icons.clear();
                        a.dock.relayout(&a.settings);
                        a.dock.render(&a.settings);
                    })
                    .is_none()
                    {
                        let _ = SetTimer(Some(hwnd), TIMER_RELAYOUT, 250, None);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_THEMECHANGED => {
            let _ = PostMessageW(Some(hwnd), WM_APP_RELAYOUT, WPARAM(0), LPARAM(0));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wp.0 != 0 {
                with_app(|a| {
                    a.restore_taskbar();
                    a.save_pending = a.persist;
                    a.save_now()
                });
            } else {
                with_app(|a| a.save_now());
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ if msg != 0 && msg == TASKBAR_CREATED => {
            // Explorer restarted: put the tray icon back and re-apply the
            // taskbar option.
            with_app(|a| {
                a.tray.add();
                a.sync_taskbar();
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn dock_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = (get_x_lparam(lp) as f32, get_y_lparam(lp) as f32);
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            with_app(|a| a.dock.on_mouse_move(x, y));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if with_app(|a| a.dock.on_button_down(x, y)) == Some(true) {
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // Work out the click *before* releasing mouse capture: releasing
            // it sends WM_CAPTURECHANGED, which cancels any pending press.
            let shift = apps::shift_down();
            let r = with_app(|a| {
                let (click, reorder) = a.dock.on_button_up(x, y);
                if let Some((from, to)) = reorder {
                    if from < a.settings.items.len() {
                        let it = a.settings.items.remove(from);
                        let to = to.min(a.settings.items.len());
                        a.settings.items.insert(to, it);
                        a.apply();
                    }
                }
                click
            });
            let _ = ReleaseCapture();
            if let Some(Some(idx)) = r {
                perform(Action::Open {
                    idx,
                    new_instance: shift,
                });
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            if let Some(Some(idx)) = with_app(|a| a.dock.hit_test(x, y)) {
                perform(Action::Open {
                    idx,
                    new_instance: true,
                });
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let idx = with_app(|a| {
                a.dock.cancel_press();
                a.dock.hit_test(x, y)
            })
            .flatten();
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            show_dock_menu(idx, pt.x, pt.y);
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            with_app(|a| a.dock.cancel_press());
            LRESULT(0)
        }
        WM_TIMER => {
            let id = wp.0;
            with_app(|a| match id {
                dock::TIMER_FRAME => a.dock.on_frame(&a.settings),
                dock::TIMER_POLL => a.dock.on_poll(&a.settings),
                _ => {}
            });
            LRESULT(0)
        }
        WM_DROPFILES => {
            let hdrop = HDROP(wp.0 as *mut _);
            let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
            let mut paths = Vec::new();
            for i in 0..count.min(config::MAX_ITEMS as u32) {
                let len = DragQueryFileW(hdrop, i, None);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                paths.push(from_wide(&buf));
            }
            let mut pt = POINT::default();
            let _ = DragQueryPoint(hdrop, &mut pt);
            DragFinish(hdrop);
            let index = with_app(|a| {
                let pinned_before = a.dock.drop_index(pt.x as f32, pt.y as f32);
                pinned_before.min(a.settings.items.len())
            });
            let items: Vec<PinnedItem> = paths.iter().map(|p| apps::item_for_path(p)).collect();
            add_items(items, index);
            LRESULT(0)
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            if let Some(c) = controller() {
                let _ = PostMessageW(Some(c), WM_APP_RELAYOUT, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ----- startup -------------------------------------------------------------

struct Args {
    #[allow(dead_code)]
    startup: bool,
    after_update: Option<u32>,
    reset: bool,
    self_test: Option<String>,
    preview: Option<String>,
    preview_dark: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        startup: false,
        after_update: None,
        reset: false,
        self_test: None,
        preview: None,
        preview_dark: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_ascii_lowercase().as_str() {
            "--startup" => a.startup = true,
            "--reset-settings" => a.reset = true,
            "--after-update" => a.after_update = it.next().and_then(|p| p.parse().ok()),
            "--self-test" => {
                a.self_test = Some(it.next().unwrap_or_else(|| "berth-self-test.txt".into()))
            }
            "--render-preview" => a.preview = it.next(),
            "--dark" => a.preview_dark = true,
            "--remove-startup" => {
                // Used when uninstalling: removes the sign-in entry and exits.
                let _ = autostart::disable();
                std::process::exit(0);
            }
            "--version" => {
                info_box(None, &format!("Berth {VERSION}"));
                std::process::exit(0);
            }
            "--help" | "-h" | "/?" => {
                info_box(
                    None,
                    "Berth command-line options:\n\n\
                     --reset-settings   Start with default settings (the old file is kept as a backup).\n\
                     --remove-startup   Stop Berth from starting at sign-in, then exit.\n\
                     --version          Show the version.\n\n\
                     Developer options:\n\
                     --self-test <file>        Run startup checks and write a report.\n\
                     --render-preview <png> [--dark]  Render the dock to an image.",
                );
                std::process::exit(0);
            }
            _ => {}
        }
    }
    a
}

unsafe fn register_class(name: PCWSTR, proc_: WNDPROC, hinst: HINSTANCE) -> bool {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc_,
        hInstance: hinst,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        hIcon: tray::app_icon(32),
        hIconSm: tray::app_icon(16),
        lpszClassName: name,
        ..Default::default()
    };
    RegisterClassExW(&wc) != 0
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("unexpected error: {info}");
        log(&msg);
        error_box(
            None,
            &format!(
                "Berth ran into a problem and has to close. The Windows taskbar is not affected.\n\n\
                 Details were written to:\n{}",
                util::log_path().display()
            ),
        );
    }));
}

fn main() {
    install_panic_hook();
    let args = parse_args();
    let testing = args.self_test.is_some() || args.preview.is_some();

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    }

    // Windows 11 only.
    match util::windows_build() {
        Some(b) if b >= util::MIN_WINDOWS_BUILD => {}
        Some(b) => {
            error_box(
                None,
                &format!(
                    "Berth requires Windows 11 (build {} or later). This PC is running build {b}.",
                    util::MIN_WINDOWS_BUILD
                ),
            );
            std::process::exit(1);
        }
        None => log("could not read the Windows build number; continuing"),
    }

    // After a self-update, let the previous version finish exiting first.
    if let Some(pid) = args.after_update {
        updater::wait_for_previous(pid);
    }

    // One dock per user session; a second launch opens the settings window.
    if !testing {
        unsafe {
            let m = CreateMutexW(None, true, w!("Local\\Berth.Dock.SingleInstance"));
            if let Ok(h) = m {
                SINGLE_INSTANCE.store(h.0 as isize, std::sync::atomic::Ordering::Relaxed);
            }
            if windows::Win32::Foundation::GetLastError()
                == windows::Win32::Foundation::ERROR_ALREADY_EXISTS
            {
                if let Ok(existing) = FindWindowW(CONTROLLER_CLASS, PCWSTR::null()) {
                    let _ =
                        PostMessageW(Some(existing), WM_APP_SHOW_SETTINGS, WPARAM(0), LPARAM(0));
                }
                return;
            }
        }
    }

    // Settings.
    let mut first_run = false;
    let mut notice: Option<String> = None;
    let settings = if testing {
        let mut s = Settings::default();
        if args.preview_dark {
            s.theme = config::ThemeMode::Dark;
        }
        s.items = apps::default_items();
        // Developer aid for previews: BERTH_PREVIEW_ITEMS="C:\a.exe;C:\b.lnk"
        if let Ok(list) = std::env::var("BERTH_PREVIEW_ITEMS") {
            s.items = list
                .split(';')
                .filter(|p| !p.trim().is_empty())
                .map(apps::item_for_path)
                .collect();
        }
        if let Ok(v) = std::env::var("BERTH_PREVIEW_POSITION") {
            s.position = match v.as_str() {
                "left" => config::Edge::Left,
                "right" => config::Edge::Right,
                _ => config::Edge::Bottom,
            };
        }
        s
    } else {
        if args.reset {
            // Put the taskbar back before forgetting what it was.
            if let config::LoadOutcome::Loaded(old) = config::load() {
                if let Some(prev) = old.taskbar_autohide_before {
                    taskbar::set_autohide(prev);
                }
            }
            if let Some(b) = config::reset_file() {
                notice = Some(format!("Berth started with default settings. Your previous settings were saved as:\n{}", b.display()));
            }
        }
        match config::load() {
            config::LoadOutcome::Loaded(s) => s,
            config::LoadOutcome::FirstRun(mut s) => {
                first_run = true;
                s.items = apps::default_items();
                if let Err(e) = config::save(&s) {
                    log(&format!("could not write first settings: {e}"));
                }
                s
            }
            config::LoadOutcome::Recovered {
                mut settings,
                backup,
                reason,
            } => {
                settings.items = apps::default_items();
                notice = Some(format!(
                    "Berth couldn't read its settings because {reason}.\n\nIt started with default settings. \
                     The unreadable file was kept as:\n{}",
                    backup.display()
                ));
                let _ = config::save(&settings);
                settings
            }
        }
    };
    log(&format!(
        "Berth {VERSION} starting (build {:?})",
        util::windows_build()
    ));

    // Windows.
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default() };
    let (controller, dock_hwnd) = unsafe {
        TASKBAR_CREATED = RegisterWindowMessageW(w!("TaskbarCreated"));
        if !register_class(CONTROLLER_CLASS, Some(controller_proc), hinst)
            || !register_class(DOCK_CLASS, Some(dock_proc), hinst)
            || !dashboard::register(hinst)
            || !picker::register(hinst)
        {
            error_box(
                None,
                "Berth couldn't start because Windows refused to register its windows.",
            );
            std::process::exit(1);
        }
        let controller = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CONTROLLER_CLASS,
            w!("Berth"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        );
        let dock = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_ACCEPTFILES,
            DOCK_CLASS,
            w!("Berth Dock"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        );
        match (controller, dock) {
            (Ok(c), Ok(d)) => (c, d),
            (c, d) => {
                let err = c
                    .err()
                    .or(d.err())
                    .map(|e| e.to_string())
                    .unwrap_or_default();
                error_box(
                    None,
                    &format!("Berth couldn't create its dock window.\n\n{err}"),
                );
                std::process::exit(1);
            }
        }
    };
    unsafe { DragAcceptFiles(dock_hwnd, true) };

    let mut dock = Dock::new(dock_hwnd);
    dock.sync_items(&settings);
    dock.relayout(&settings);
    let app = App {
        settings,
        controller,
        dock,
        tray: tray::Tray::new(controller),
        save_pending: false,
        persist: !testing,
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));

    if let Some(path) = &args.preview {
        let code = render_preview(path);
        std::process::exit(code);
    }
    if let Some(report) = &args.self_test {
        let code = self_test(report);
        std::process::exit(code);
    }

    with_app(|a| {
        a.tray.add();
        a.sync_taskbar();
        a.dock.render(&a.settings);
        a.dock.start_poll_timer();
        unsafe {
            SetTimer(
                Some(a.controller),
                TIMER_UPDATE_CHECK,
                UPDATE_CHECK_INTERVAL_MS,
                None,
            )
        };
        if first_run {
            a.tray.notify(
                "Berth is running",
                "Click this icon to open the Berth Dashboard. The Windows taskbar stays available.",
            );
        }
        if let Some(err) = &a.dock.last_error {
            log(&format!("first frame failed: {err}"));
        }
    });
    autostart::refresh_path_if_enabled();
    std::thread::spawn(updater::cleanup);
    let migrated = config::MIGRATED_TO_V2.load(std::sync::atomic::Ordering::Relaxed);
    if args.after_update.is_some() || (migrated && !first_run) {
        log(&format!("now running Berth {VERSION} after an update"));
        let body = if migrated {
            format!("{WHATS_NEW} Update notifications are now on; turn them off in Dashboard > Updates.")
        } else {
            WHATS_NEW.to_string()
        };
        with_app(|a| {
            if migrated {
                a.schedule_save();
            }
            a.tray.notify(&format!("Berth updated to {VERSION}"), &body)
        });
    } else if with_app(|a| a.settings.check_updates_on_startup) == Some(true) {
        {
            let ctl = controller;
            updater::start_check(ctl, WM_APP_UPDATE, true);
        }
    }
    if let Some(n) = notice {
        util::warn_box(None, &n);
    }
    if first_run {
        dashboard::first_run_welcome();
    }

    unsafe {
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 || r.0 == -1 {
                break;
            }
            if dashboard::dialog_message(&msg) || picker::dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    with_app(|a| {
        let _ = a.save_now();
        a.tray.remove();
    });
    log("Berth exited");
}

/// `--render-preview out.png [--dark]`: draws the dock with one icon
/// hovered and saves the image. Used for screenshots and visual checks.
fn render_preview(path: &str) -> i32 {
    let r = with_app(|a| {
        a.dock.animate = false;
        let idx = a.dock.entries.len().min(3).saturating_sub(1);
        a.dock.preview_hover(idx);
        if !a.dock.paint(&a.settings) {
            return Err("render failed".to_string());
        }
        a.dock.save_png(path)
    });
    match r {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            log(&format!("preview failed: {e}"));
            1
        }
        None => 1,
    }
}

/// `--self-test report.txt`: exercises startup paths without touching the
/// user's settings and writes a plain-text report. Exit code 0 = pass.
fn self_test(report: &str) -> i32 {
    let mut lines = vec![
        format!("Berth {VERSION} self-test"),
        format!("windows build: {:?}", util::windows_build()),
    ];
    let mut ok = true;
    let mons = system::monitors();
    lines.push(format!("monitors: {}", mons.len()));
    for (i, m) in mons.iter().enumerate() {
        lines.push(format!("  {} {} dpi={}", m.label(i), m.device, m.dpi));
    }
    ok &= !mons.is_empty();
    let res = with_app(|a| {
        a.dock.render(&a.settings);
        let painted = a.dock.paint(&a.settings);
        let geo = a.dock.geo.as_ref().map(|g| {
            format!(
                "{}x{} at ({}, {}) icon={}px",
                g.w, g.h, g.win.left, g.win.top, g.s
            )
        });
        (
            painted,
            geo,
            a.dock.last_error.clone(),
            a.dock
                .entries
                .iter()
                .map(|e| {
                    format!(
                        "{} -> {} (exists={}, running={})",
                        e.item.name,
                        e.item.target,
                        e.keys.exists,
                        e.running()
                    )
                })
                .collect::<Vec<_>>(),
        )
    });
    match res {
        Some((painted, geo, err, entries)) => {
            lines.push(format!("painted: {painted}"));
            lines.push(format!(
                "geometry: {}",
                geo.unwrap_or_else(|| "none".into())
            ));
            lines.push(format!(
                "present error: {}",
                err.clone().unwrap_or_else(|| "none".into())
            ));
            lines.push(format!("default items: {}", entries.len()));
            lines.extend(entries.into_iter().map(|e| format!("  {e}")));
            ok &= painted && err.is_none();
        }
        None => ok = false,
    }
    let wins = apps::app_windows(std::process::id());
    lines.push(format!("taskbar-style windows: {}", wins.len()));
    let installed = apps::installed_apps();
    lines.push(format!("installed apps listed: {}", installed.len()));
    lines.push(format!(
        "theme dark: {}  animations: {}",
        system::system_prefers_dark(),
        system::system_animations_enabled()
    ));
    lines.push(format!("autostart: {:?}", autostart::state()));
    // CI only (it briefly changes the taskbar): exercise the taskbar option
    // the same way the dashboard does, then put everything back.
    if std::env::var("BERTH_TEST_TASKBAR").as_deref() == Ok("1") {
        match taskbar::is_autohide() {
            None => lines.push("taskbar: not running (skipped)".into()),
            Some(orig) => {
                let wait = || std::thread::sleep(std::time::Duration::from_millis(700));
                taskbar::set_autohide(true);
                wait();
                let hidden = taskbar::is_autohide() == Some(true);
                taskbar::set_autohide(orig);
                wait();
                let restored = taskbar::is_autohide() == Some(orig);
                lines.push(format!(
                    "taskbar: auto-hide was {orig}; turned on: {hidden}; restored: {restored}"
                ));
                ok &= hidden && restored;
            }
        }
    }
    lines.push(format!("result: {}", if ok { "PASS" } else { "FAIL" }));
    let _ = std::fs::write(report, lines.join("\r\n"));
    if ok {
        0
    } else {
        1
    }
}
