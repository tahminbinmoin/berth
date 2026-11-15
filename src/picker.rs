//! "Add app" window (lists everything in Start > All apps, including Store
//! apps) and the file/folder browse dialog.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, HSTRING, PWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetSysColor, GetSysColorBrush, SetBkColor, COLOR_WINDOW, HDC, HFONT,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Controls::{LVCOLUMNW, LVITEMW, NMHDR, NMLVDISPINFOW};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_ALLOWMULTISELECT,
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NODEREFERENCELINKS, FOS_PICKFOLDERS,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::apps::{self, InstalledApp};
use crate::config::PinnedItem;
use crate::ui::{self, c, child, send};
use crate::{system, util, with_app};

const CLASS: windows::core::PCWSTR = w!("BerthAddApp");
const CW: i32 = 460;
const CH: i32 = 560;

const SEARCH: u16 = 10;
const LIST: u16 = 11;
const BROWSE: u16 = 12;
const ADD: u16 = 13;
const CLOSE: u16 = 14;
const HINT: u16 = 15;

struct Picker {
    hwnd: HWND,
    font: HFONT,
    list: HWND,
    search: HWND,
    add: HWND,
    apps: Vec<InstalledApp>,
    shown: Vec<usize>,
    icons: HashMap<usize, i32>,
}

thread_local! {
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with_picker<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICKER.with(|p| {
        let mut b = p.try_borrow_mut().ok()?;
        Some(f(b.as_mut()?))
    })
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

pub fn dialog_message(msg: &MSG) -> bool {
    match with_picker(|p| p.hwnd) {
        Some(h) => unsafe { IsDialogMessageW(h, msg).as_bool() },
        None => false,
    }
}

pub fn close() {
    if let Some(h) = with_picker(|p| p.hwnd) {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

pub fn open() {
    if let Some(h) = with_picker(|p| p.hwnd) {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
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
    let style = (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU).0;
    let r = ui::centered_window_rect(work, CW, CH, dpi as f32 / 96.0, style, 0, dpi);
    let Ok(h) = (unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0x10000),
            CLASS,
            w!("Add apps to the dock"),
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
    }) else {
        util::error_box(None, "Berth couldn't open the Add app window.");
        return;
    };
    let dpi = ui::dpi_of(h);
    let scale = dpi as f32 / 96.0;
    let font = ui::message_font(dpi, 1.0, 0);
    let hint = child(
        h,
        "STATIC",
        "Choose one or more apps. Hold Ctrl to select several.",
        c::SS_NOPREFIX,
        0,
        (16, 14, CW - 32, 22),
        HINT,
        font,
        scale,
    );
    let _ = hint;
    let search = child(
        h,
        "EDIT",
        "",
        c::ES_AUTOHSCROLL,
        0x200,
        (16, 42, CW - 32, 28),
        SEARCH,
        font,
        scale,
    );
    send(
        search,
        c::EM_SETCUEBANNER,
        1,
        HSTRING::from("Search apps").as_ptr() as isize,
    );
    let list = child(
        h,
        "SysListView32",
        "",
        c::LVS_REPORT | c::LVS_SHOWSELALWAYS | c::LVS_SHAREIMAGELISTS | c::LVS_NOCOLUMNHEADER,
        0x200,
        (16, 80, CW - 32, CH - 140),
        LIST,
        font,
        scale,
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
        apps::system_image_list(),
    );
    let col_text = HSTRING::from("App");
    let col = LVCOLUMNW {
        mask: windows::Win32::UI::Controls::LVCOLUMNW_MASK(c::LVCF_TEXT | c::LVCF_WIDTH),
        cx: ((CW - 60) as f32 * scale) as i32,
        pszText: PWSTR(col_text.as_ptr() as *mut u16),
        ..Default::default()
    };
    send(list, c::LVM_INSERTCOLUMNW, 0, &col as *const _ as isize);
    child(
        h,
        "BUTTON",
        "Browse for a file\u{2026}",
        c::BS_PUSHBUTTON,
        0,
        (16, CH - 48, 150, 30),
        BROWSE,
        font,
        scale,
    );
    let add = child(
        h,
        "BUTTON",
        "Add to dock",
        c::BS_PUSHBUTTON,
        0,
        (CW - 16 - 96 - 8 - 110, CH - 48, 110, 30),
        ADD,
        font,
        scale,
    );
    child(
        h,
        "BUTTON",
        "Close",
        c::BS_PUSHBUTTON,
        0,
        (CW - 16 - 96, CH - 48, 96, 30),
        CLOSE,
        font,
        scale,
    );

    unsafe {
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
        let _ = SetCursor(LoadCursorW(None, IDC_WAIT).ok());
    }
    let list_apps = apps::installed_apps();
    PICKER.with(|p| {
        *p.borrow_mut() = Some(Picker {
            hwnd: h,
            font,
            list,
            search,
            add,
            apps: list_apps,
            shown: Vec::new(),
            icons: HashMap::new(),
        })
    });
    if with_picker(|p| p.apps.is_empty()) == Some(true) {
        util::warn_box(
            Some(h),
            "Berth couldn't read the list of installed apps. You can still add apps with \u{201c}Browse for a file\u{2026}\u{201d} or by dragging shortcuts onto the dock.",
        );
    }
    filter();
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(search));
    }
}

fn filter() {
    let Some((list, search, apps)) = with_picker(|p| (p.list, p.search, p.apps.clone())) else {
        return;
    };
    let q = ui::get_text(search).to_lowercase();
    let shown: Vec<usize> = apps
        .iter()
        .enumerate()
        .filter(|(_, a)| q.is_empty() || a.name.to_lowercase().contains(q.trim()))
        .map(|(i, _)| i)
        .collect();
    send(list, c::LVM_DELETEALLITEMS, 0, 0);
    for (row, &i) in shown.iter().enumerate() {
        let name = HSTRING::from(apps[i].name.as_str());
        let mut item = LVITEMW {
            mask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_FLAGS(
                c::LVIF_TEXT | c::LVIF_IMAGE | c::LVIF_PARAM,
            ),
            iItem: row as i32,
            pszText: PWSTR(name.as_ptr() as *mut u16),
            iImage: c::I_IMAGECALLBACK,
            lParam: LPARAM(i as isize),
            ..Default::default()
        };
        send(list, c::LVM_INSERTITEMW, 0, &mut item as *mut _ as isize);
    }
    with_picker(|p| p.shown = shown);
    update_add_button();
}

fn selected_rows() -> Vec<usize> {
    let Some(list) = with_picker(|p| p.list) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = usize::MAX;
    loop {
        let r = send(list, c::LVM_GETNEXTITEM, i, c::LVNI_SELECTED as isize);
        if r < 0 {
            break;
        }
        out.push(r as usize);
        i = r as usize;
    }
    out
}

fn update_add_button() {
    let any = !selected_rows().is_empty();
    if let Some(b) = with_picker(|p| p.add) {
        unsafe {
            let _ = EnableWindow(b, any);
        }
    }
}

fn add_selected() {
    let rows = selected_rows();
    let items: Vec<PinnedItem> = with_picker(|p| {
        rows.iter()
            .filter_map(|r| p.shown.get(*r).and_then(|i| p.apps.get(*i)))
            .map(|a| PinnedItem {
                name: a.name.clone(),
                target: a.target.clone(),
            })
            .collect()
    })
    .unwrap_or_default();
    if items.is_empty() {
        return;
    }
    let requested = items.len();
    let added = crate::add_items(items, None);
    if added < requested {
        let skipped = requested - added;
        util::info_box(
            with_picker(|p| p.hwnd),
            &format!(
                "{added} added. {skipped} {} already on the dock (or the dock is full).",
                if skipped == 1 { "was" } else { "were" }
            ),
        );
    }
    close();
}

/// Shows the standard Windows file (or folder) picker. Shortcuts are kept
/// as shortcuts rather than resolved to their targets.
pub fn browse(owner: Option<HWND>, folders: bool) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let Ok(dlg) =
            CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
        else {
            util::error_box(owner, "Windows couldn't open the file picker.");
            return out;
        };
        let mut opts = dlg.GetOptions().unwrap_or_default();
        opts |= FOS_FORCEFILESYSTEM | FOS_ALLOWMULTISELECT;
        if folders {
            opts |= FOS_PICKFOLDERS;
            let _ = dlg.SetTitle(w!("Choose folders to add to the dock"));
        } else {
            opts |= FOS_FILEMUSTEXIST | FOS_NODEREFERENCELINKS;
            let _ = dlg.SetTitle(w!("Choose apps, shortcuts or files to add to the dock"));
            let specs = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Apps and shortcuts"),
                    pszSpec: w!("*.exe;*.lnk;*.url;*.appref-ms;*.bat;*.cmd"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All files"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = dlg.SetFileTypes(&specs);
            if let Ok(start) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from("shell:Common Programs"),
                None,
            ) {
                let _ = dlg.SetFolder(&start);
            }
        }
        let _ = dlg.SetOptions(opts);
        if dlg.Show(owner).is_err() {
            return out; // cancelled
        }
        if let Ok(results) = dlg.GetResults() {
            let n = results.GetCount().unwrap_or(0);
            for i in 0..n {
                if let Ok(item) = results.GetItemAt(i) {
                    if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                        let s = util::take_pwstr(p);
                        if !s.is_empty() {
                            out.push(s);
                        }
                    }
                }
            }
        }
    }
    out
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let (id, code) = (util::loword(wp.0), util::hiword(wp.0));
            match (id, code) {
                (SEARCH, c::EN_CHANGE) => filter(),
                (ADD, c::BN_CLICKED) | (c::IDOK, _) => add_selected(),
                (BROWSE, c::BN_CLICKED) => {
                    let paths = browse(Some(hwnd), false);
                    let items: Vec<PinnedItem> =
                        paths.iter().map(|p| apps::item_for_path(p)).collect();
                    if !items.is_empty() {
                        crate::add_items(items, None);
                        close();
                    }
                }
                (CLOSE, c::BN_CLICKED) | (c::IDCANCEL, _) => close(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = &*(lp.0 as *const NMHDR);
            if hdr.idFrom == LIST as usize {
                match hdr.code as i32 {
                    c::LVN_ITEMCHANGED => update_add_button(),
                    c::NM_DBLCLK | c::NM_RETURN => add_selected(),
                    c::LVN_GETDISPINFOW => {
                        let info = &mut *(lp.0 as *mut NMLVDISPINFOW);
                        if info.item.mask.0 & c::LVIF_IMAGE != 0 {
                            let row = info.item.iItem as usize;
                            let idx = with_picker(|p| {
                                let app_i = *p.shown.get(row)?;
                                if let Some(i) = p.icons.get(&app_i) {
                                    return Some(*i);
                                }
                                let i = apps::system_icon_index(&p.apps[app_i].target);
                                p.icons.insert(app_i, i);
                                Some(i)
                            })
                            .flatten()
                            .unwrap_or(-1);
                            info.item.iImage = idx.max(0);
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            SetBkColor(
                HDC(wp.0 as *mut _),
                windows::Win32::Foundation::COLORREF(GetSysColor(COLOR_WINDOW)),
            );
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(f) = with_picker(|p| p.font) {
                ui::delete_font(f);
            }
            PICKER.with(|p| {
                if let Ok(mut b) = p.try_borrow_mut() {
                    *b = None;
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
