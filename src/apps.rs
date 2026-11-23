//! Launching pinned items, finding their running windows, and listing the
//! apps installed for the current user.

use std::collections::HashMap;

use windows::core::{Interface, BOOL, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HWND, LPARAM, MAX_PATH};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, IShellItem2, IShellLinkW,
    SHCreateItemFromParsingName, ShellExecuteExW, ShellLink, SEE_MASK_FLAG_NO_UI,
    SHELLEXECUTEINFOW, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SLGP_UNCPRIORITY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumChildWindows, EnumWindows, FlashWindowEx, GetClassNameW,
    GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, ASFW_ANY, FLASHWINFO, FLASHW_ALL,
    FLASHW_TIMERNOFG, GWL_EXSTYLE, GW_OWNER, SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use crate::config::{self, PinnedItem};
use crate::util::{self, from_wide, take_pwstr};

const fn pkey(d1: u32, d2: u16, d3: u16, d4: [u8; 8], pid: u32) -> PROPERTYKEY {
    PROPERTYKEY {
        fmtid: GUID {
            data1: d1,
            data2: d2,
            data3: d3,
            data4: d4,
        },
        pid,
    }
}
/// System.AppUserModel.ID
const PKEY_APPUSERMODEL_ID: PROPERTYKEY = pkey(
    0x9F4C2855,
    0x9F79,
    0x4B39,
    [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3],
    5,
);
/// System.Link.TargetParsingPath
const PKEY_LINK_TARGET_PARSING_PATH: PROPERTYKEY = pkey(
    0xB9B4B3FC,
    0x2B51,
    0x4A42,
    [0xB5, 0xD8, 0x32, 0x41, 0x46, 0xAF, 0xCF, 0x25],
    2,
);
/// System.FileDescription
const PKEY_FILE_DESCRIPTION: PROPERTYKEY = pkey(
    0x0CEF7D53,
    0xFA64,
    0x11D1,
    [0xA2, 0x03, 0x00, 0x00, 0xF8, 0x1F, 0xED, 0xEE],
    3,
);

pub const APPS_FOLDER_PREFIX: &str = r"shell:AppsFolder\";

/// Keys used to recognize an item's windows.
#[derive(Clone, Debug, Default)]
pub struct MatchKeys {
    /// Lower-case full path of the executable, when known.
    pub exe: Option<String>,
    /// Lower-case AppUserModelID, when known.
    pub aumid: Option<String>,
    /// File Explorer windows are matched by window class.
    pub explorer: bool,
    /// Whether the target currently exists.
    pub exists: bool,
}

fn is_shell_location(target: &str) -> bool {
    target.len() > 6 && target[..6].eq_ignore_ascii_case("shell:")
}

fn path_exists(path: &str) -> bool {
    unsafe { GetFileAttributesW(&HSTRING::from(path)) != INVALID_FILE_ATTRIBUTES }
}

unsafe fn item_string(item: &IShellItem2, key: &PROPERTYKEY) -> Option<String> {
    let p = item.GetString(key).ok()?;
    let s = take_pwstr(p);
    (!s.is_empty()).then_some(s)
}

unsafe fn shortcut_target(path: &str) -> Option<String> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
    let file: IPersistFile = link.cast().ok()?;
    file.Load(&HSTRING::from(path), STGM_READ).ok()?;
    let mut buf = vec![0u16; MAX_PATH as usize * 2];
    link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_UNCPRIORITY.0 as u32)
        .ok()?;
    let s = from_wide(&buf);
    (!s.is_empty()).then_some(s)
}

/// Works out how to recognize the windows that belong to a pinned target.
pub fn resolve(target: &str) -> MatchKeys {
    let mut keys = MatchKeys::default();
    let lower = target.to_ascii_lowercase();
    unsafe {
        let item: Option<IShellItem2> =
            SHCreateItemFromParsingName(&HSTRING::from(target), None).ok();
        keys.exists = if is_shell_location(target) {
            item.is_some()
        } else {
            path_exists(target)
        };

        if let Some(item) = &item {
            if let Some(id) = item_string(item, &PKEY_APPUSERMODEL_ID) {
                keys.aumid = Some(id.to_ascii_lowercase());
            }
            if let Some(t) = item_string(item, &PKEY_LINK_TARGET_PARSING_PATH) {
                if t.to_ascii_lowercase().ends_with(".exe") {
                    keys.exe = Some(t.to_ascii_lowercase());
                }
            }
        }
        if lower.starts_with(&APPS_FOLDER_PREFIX.to_ascii_lowercase()) && keys.aumid.is_none() {
            let id = &target[APPS_FOLDER_PREFIX.len()..];
            if !id.contains('\\') {
                keys.aumid = Some(id.to_ascii_lowercase());
            }
        }
        if lower.ends_with(".exe") && !is_shell_location(target) {
            keys.exe = Some(lower.clone());
        }
        if lower.ends_with(".lnk") && keys.exe.is_none() {
            if let Some(t) = shortcut_target(target) {
                if t.to_ascii_lowercase().ends_with(".exe") {
                    keys.exe = Some(t.to_ascii_lowercase());
                }
            }
        }
    }
    let explorer_exe = keys
        .exe
        .as_deref()
        .map(|e| e.ends_with(r"\explorer.exe"))
        .unwrap_or(false);
    let explorer_id = keys.aumid.as_deref() == Some("microsoft.windows.explorer");
    if explorer_exe || explorer_id {
        keys.explorer = true;
    }
    keys
}

/// A top-level window that would appear on the taskbar.
#[derive(Clone, Debug)]
pub struct AppWindow {
    pub hwnd: HWND,
    pub exe: String,
    pub aumid: Option<String>,
    /// AUMID with original casing (used to relaunch packaged apps).
    pub aumid_raw: Option<String>,
    pub class: String,
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    from_wide(&buf[..n.max(0) as usize])
}

fn process_image(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn process_aumid(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 512];
        let mut len = buf.len() as u32;
        let r = GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(h);
        if r.is_ok() {
            Some(from_wide(&buf))
        } else {
            None
        }
    }
}

fn window_aumid(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let v = store.GetValue(&PKEY_APPUSERMODEL_ID).ok()?;
        let p = PropVariantToStringAlloc(&v).ok()?;
        let s = take_pwstr(p);
        (!s.is_empty()).then_some(s)
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok()
            && cloaked != 0
    }
}

unsafe extern "system" fn collect_hwnd(hwnd: HWND, data: LPARAM) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<HWND>);
    list.push(hwnd);
    BOOL(1)
}

/// Finds the packaged app hosted inside an ApplicationFrameHost window.
fn hosted_core_window_pid(frame: HWND, frame_pid: u32) -> Option<u32> {
    let mut children: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(
            Some(frame),
            Some(collect_hwnd),
            LPARAM(&mut children as *mut _ as isize),
        );
    }
    for c in children {
        if class_of(c) == "Windows.UI.Core.CoreWindow" {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(c, Some(&mut pid)) };
            if pid != 0 && pid != frame_pid {
                return Some(pid);
            }
        }
    }
    None
}

/// Lists windows the way the taskbar would show them, top of the z-order first.
pub fn app_windows(own_pid: u32) -> Vec<AppWindow> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_hwnd), LPARAM(&mut all as *mut _ as isize));
    }
    let mut exe_cache: HashMap<u32, Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for hwnd in all {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                continue;
            }
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            if !owner.0.is_null() && ex & WS_EX_APPWINDOW.0 == 0 {
                continue;
            }
            if GetWindowTextLengthW(hwnd) == 0 || is_cloaked(hwnd) {
                continue;
            }
            let class = class_of(hwnd);
            if matches!(
                class.as_str(),
                "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
            ) {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == own_pid {
                continue;
            }
            let mut exe = exe_cache
                .entry(pid)
                .or_insert_with(|| process_image(pid))
                .clone();
            let mut aumid_raw = window_aumid(hwnd);
            if exe.as_deref().map(|e| {
                e.to_ascii_lowercase()
                    .ends_with(r"\applicationframehost.exe")
            }) == Some(true)
            {
                // A packaged (Store) app: look through the frame to the real app.
                match hosted_core_window_pid(hwnd, pid) {
                    Some(app_pid) => {
                        exe = process_image(app_pid);
                        if aumid_raw.is_none() {
                            aumid_raw = process_aumid(app_pid);
                        }
                    }
                    None if aumid_raw.is_none() => continue,
                    None => exe = None,
                }
            } else if aumid_raw.is_none() {
                aumid_raw = process_aumid(pid);
            }
            out.push(AppWindow {
                hwnd,
                exe: exe.unwrap_or_default().to_ascii_lowercase(),
                aumid: aumid_raw.as_ref().map(|a| a.to_ascii_lowercase()),
                aumid_raw,
                class,
            });
        }
    }
    out
}

pub fn window_matches(keys: &MatchKeys, w: &AppWindow) -> bool {
    if keys.explorer {
        return w.class == "CabinetWClass";
    }
    if let (Some(a), Some(b)) = (&keys.aumid, &w.aumid) {
        if b == a || (b.starts_with(a.as_str()) && b.as_bytes().get(a.len()) == Some(&b'.')) {
            return true;
        }
    }
    if let Some(e) = &keys.exe {
        if !w.exe.is_empty() && *e == w.exe {
            return true;
        }
    }
    false
}

pub enum LaunchError {
    Missing,
    Cancelled,
    Other(String),
}

pub fn shift_down() -> bool {
    unsafe { (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 }
}

/// Opens a target the same way Explorer would (default verb).
pub fn launch(target: &str) -> Result<(), LaunchError> {
    if !is_shell_location(target) && !path_exists(target) {
        return Err(LaunchError::Missing);
    }
    let file = util::wide(target);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        match ShellExecuteExW(&mut info) {
            Ok(()) => Ok(()),
            Err(e) => {
                let code = (e.code().0 as u32) & 0xffff;
                if code == ERROR_CANCELLED.0 {
                    Err(LaunchError::Cancelled)
                } else if code == 2 || code == 3 {
                    Err(LaunchError::Missing)
                } else {
                    Err(LaunchError::Other(e.message().to_string()))
                }
            }
        }
    }
}

/// Opens File Explorer with the target selected.
pub fn reveal_in_explorer(target: &str) {
    let file = util::wide("explorer.exe");
    let params = util::wide(&format!("/select,\"{}\"", target));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        if let Err(e) = ShellExecuteExW(&mut info) {
            util::log(&format!("could not open file location for {target}: {e}"));
        }
    }
}

/// Brings an app's windows forward. Clicking an app that is already in front
/// cycles through its windows, or minimizes it when it has just one
/// (matching the Windows taskbar).
pub fn activate(windows: &[HWND]) {
    if windows.is_empty() {
        return;
    }
    unsafe {
        let fg = GetForegroundWindow();
        let target = match windows.iter().position(|&w| w == fg) {
            Some(_) if windows.len() == 1 => {
                let _ = ShowWindow(fg, SW_MINIMIZE);
                return;
            }
            // EnumWindows lists the front-most window first, so the next one
            // in the list is the most recently used of the others.
            Some(i) => windows[(i + 1) % windows.len()],
            None => windows[0],
        };
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        if !SetForegroundWindow(target).as_bool() {
            // Windows may refuse to switch focus; flash the window instead of
            // trying to work around the foreground lock.
            let fi = FLASHWINFO {
                cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                hwnd: target,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 3,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&fi);
        }
    }
}

/// A friendly name for a file or shell location.
pub fn friendly_name(target: &str) -> String {
    unsafe {
        if let Ok(item) =
            SHCreateItemFromParsingName::<_, _, IShellItem2>(&HSTRING::from(target), None)
        {
            if target.to_ascii_lowercase().ends_with(".exe") {
                if let Some(d) = item_string(&item, &PKEY_FILE_DESCRIPTION) {
                    return d;
                }
            }
            if let Ok(p) = item.GetDisplayName(SIGDN_NORMALDISPLAY) {
                let s = take_pwstr(p);
                if !s.is_empty() {
                    return strip_known_ext(&s);
                }
            }
        }
    }
    config::display_name_from_target(target)
}

fn strip_known_ext(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for ext in [".exe", ".lnk", ".url", ".appref-ms"] {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name.to_string()
}

pub fn item_for_path(path: &str) -> PinnedItem {
    PinnedItem {
        name: friendly_name(path),
        target: path.to_string(),
    }
}

/// An entry in the "Add app" list.
#[derive(Clone, Debug)]
pub struct InstalledApp {
    pub name: String,
    pub target: String,
}

/// Lists everything in the Start menu's "All apps" (desktop and Store apps)
/// via the `shell:AppsFolder` virtual folder.
pub fn installed_apps() -> Vec<InstalledApp> {
    let mut out = Vec::new();
    unsafe {
        let folder: IShellItem =
            match SHCreateItemFromParsingName(&HSTRING::from("shell:AppsFolder"), None) {
                Ok(f) => f,
                Err(e) => {
                    util::log(&format!("could not open shell:AppsFolder: {e}"));
                    return out;
                }
            };
        let items: IEnumShellItems = match folder.BindToHandler(None, &BHID_EnumItems) {
            Ok(i) => i,
            Err(e) => {
                util::log(&format!("could not enumerate shell:AppsFolder: {e}"));
                return out;
            }
        };
        loop {
            let mut fetched = 0u32;
            let mut slot: [Option<IShellItem>; 1] = [None];
            if items.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = slot[0].take() else { break };
            let name = item
                .GetDisplayName(SIGDN_NORMALDISPLAY)
                .map(|p| take_pwstr(p))
                .unwrap_or_default();
            let id = item
                .GetDisplayName(SIGDN_PARENTRELATIVEPARSING)
                .map(|p| take_pwstr(p))
                .unwrap_or_default();
            if name.is_empty() || id.is_empty() {
                continue;
            }
            out.push(InstalledApp {
                name,
                target: format!("{APPS_FOLDER_PREFIX}{id}"),
            });
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.target.eq_ignore_ascii_case(&b.target));
    out
}

/// Sensible first-run pins; only the ones present on this PC are kept.
pub fn default_items() -> Vec<PinnedItem> {
    let candidates = [
        (
            "File Explorer",
            r"shell:AppsFolder\Microsoft.Windows.Explorer",
        ),
        ("This PC", "shell:MyComputerFolder"),
        ("Microsoft Edge", r"shell:AppsFolder\MSEdge"),
        (
            "Notepad",
            r"shell:AppsFolder\Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
        ),
        (
            "Calculator",
            r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
        ),
        (
            "Settings",
            r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
        ),
        ("Recycle Bin", "shell:RecycleBinFolder"),
    ];
    let mut items = Vec::new();
    for (name, target) in candidates {
        let ok = unsafe {
            SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(target), None).is_ok()
        };
        if ok {
            items.push(PinnedItem {
                name: name.into(),
                target: target.into(),
            });
        }
    }
    items
}

/// Same, for the large/extra-large/jumbo image lists.
pub fn system_icon_index_large(target: &str) -> i32 {
    icon_index(target, false)
}

fn icon_index(target: &str, small: bool) -> i32 {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        ILFree, SHGetFileInfoW, SHParseDisplayName, SHFILEINFOW, SHGFI_PIDL, SHGFI_SMALLICON,
        SHGFI_SYSICONINDEX,
    };
    unsafe {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if SHParseDisplayName(&HSTRING::from(target), None, &mut pidl, 0, None).is_err()
            || pidl.is_null()
        {
            return -1;
        }
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            PCWSTR(pidl as *const u16),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            if small {
                SHGFI_PIDL | SHGFI_SYSICONINDEX | SHGFI_SMALLICON
            } else {
                SHGFI_PIDL | SHGFI_SYSICONINDEX
            },
        );
        ILFree(Some(pidl));
        if r == 0 {
            -1
        } else {
            sfi.iIcon
        }
    }
}
