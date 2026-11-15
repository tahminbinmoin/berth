//! Notification-area (system tray) icon.

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO,
    NIIF_NOSOUND, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR};

use crate::util;

/// Loads the embedded app icon (resource ID 1) at the given size.
pub fn app_icon(size: i32) -> HICON {
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else {
            return HICON::default();
        };
        LoadImageW(
            Some(hinst.into()),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    }
}

fn copy_into(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

pub struct Tray {
    hwnd: HWND,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        Tray { hwnd, added: false }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        };
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        d
    }

    pub fn add(&mut self) {
        let mut d = self.data();
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = crate::WM_APP_TRAY;
        let size = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CXSMICON,
            )
        };
        d.hIcon = app_icon(size.max(16));
        copy_into(&mut d.szTip, "Berth dock");
        unsafe {
            // After an Explorer restart the old icon is gone; ignore errors.
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            if Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
                self.added = true;
                let v = self.data();
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &v);
            } else {
                self.added = false;
                util::log("could not add the notification-area icon (will retry when the taskbar restarts)");
            }
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            }
            self.added = false;
        }
    }

    /// A one-time informational notification.
    pub fn notify(&self, title: &str, body: &str) {
        if !self.added {
            return;
        }
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, body);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }
}
