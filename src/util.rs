//! Small helpers shared across the app: wide strings, paths, logging,
//! message boxes and Windows version checks.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDOK, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING,
    MB_OK, MB_OKCANCEL, MB_SETFOREGROUND, MB_YESNO, MESSAGEBOX_STYLE,
};

pub const APP_NAME: &str = "Berth";

/// Null-terminated UTF-16 buffer.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Converts a UTF-16 buffer (optionally null-terminated) to a String.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Takes ownership of a CoTaskMem-allocated string.
pub unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

pub fn loword(v: usize) -> u16 {
    (v & 0xffff) as u16
}
pub fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xffff) as u16
}
pub fn get_x_lparam(l: LPARAM) -> i32 {
    (l.0 & 0xffff) as i16 as i32
}
pub fn get_y_lparam(l: LPARAM) -> i32 {
    ((l.0 >> 16) & 0xffff) as i16 as i32
}
pub fn get_x_wparam(w: WPARAM) -> i32 {
    (w.0 & 0xffff) as i16 as i32
}
pub fn get_y_wparam(w: WPARAM) -> i32 {
    ((w.0 >> 16) & 0xffff) as i16 as i32
}

/// `%LOCALAPPDATA%\Berth`, created on demand.
pub fn data_dir() -> PathBuf {
    let base = unsafe {
        SHGetKnownFolderPath(&FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG(0), None)
            .map(|p| take_pwstr(p))
            .ok()
    }
    .filter(|s| !s.is_empty())
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
    .unwrap_or_else(|| std::env::temp_dir());
    let dir = base.join(APP_NAME);
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn log_path() -> PathBuf {
    data_dir().join("berth.log")
}

pub fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// Appends one line to the local log file. Logging never fails loudly.
pub fn log(msg: &str) {
    let path = log_path();
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 512 * 1024 {
            let _ = fs::rename(&path, path.with_extension("log.old"));
        }
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {}", timestamp(), msg);
    }
}

fn message(hwnd: Option<HWND>, text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    unsafe {
        MessageBoxW(
            hwnd,
            &HSTRING::from(text),
            &HSTRING::from(APP_NAME),
            style | MB_SETFOREGROUND,
        )
        .0
    }
}

pub fn error_box(hwnd: Option<HWND>, text: &str) {
    log(&format!("error shown: {text}"));
    message(hwnd, text, MB_OK | MB_ICONERROR);
}

pub fn warn_box(hwnd: Option<HWND>, text: &str) {
    message(hwnd, text, MB_OK | MB_ICONWARNING);
}

pub fn info_box(hwnd: Option<HWND>, text: &str) {
    message(hwnd, text, MB_OK | MB_ICONINFORMATION);
}

pub fn confirm_box(hwnd: Option<HWND>, text: &str) -> bool {
    message(hwnd, text, MB_OKCANCEL | MB_ICONWARNING) == IDOK.0
}

pub fn ask_box(hwnd: Option<HWND>, text: &str) -> bool {
    message(hwnd, text, MB_YESNO | MB_ICONQUESTION) == IDYES.0
}

pub fn yes_no_box(hwnd: Option<HWND>, text: &str) -> bool {
    message(hwnd, text, MB_YESNO | MB_ICONWARNING) == IDYES.0
}

/// Reads a REG_SZ value under HKLM.
fn read_hklm_string(subkey: &str, value: &str) -> Option<String> {
    let mut buf = vec![0u16; 64];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r.is_ok() {
        Some(from_wide(&buf))
    } else {
        None
    }
}

/// Windows build number (e.g. 22631), if it can be determined.
pub fn windows_build() -> Option<u32> {
    read_hklm_string(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .and_then(|s| s.trim().parse().ok())
}

/// Windows 11 is build 22000 and later.
pub const MIN_WINDOWS_BUILD: u32 = 22000;

/// Path of the running executable.
pub fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}
