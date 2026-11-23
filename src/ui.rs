//! Shared helpers for Berth's native windows (Dashboard and Add app).

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HFONT, HGDIOBJ};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageW, SetWindowTextW, WINDOW_EX_STYLE, WINDOW_STYLE,
};

/// Raw Win32 constants for common controls (kept numeric to avoid type
/// juggling between the many typed wrappers).
pub mod c {
    pub const EM_SETCUEBANNER: u32 = 0x1501;
    pub const EN_CHANGE: u16 = 0x300;
}

pub fn send(h: HWND, msg: u32, w: usize, l: isize) -> isize {
    unsafe { SendMessageW(h, msg, Some(WPARAM(w)), Some(LPARAM(l))).0 }
}

pub fn dpi_of(hwnd: HWND) -> u32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 {
        96
    } else {
        d
    }
}

pub fn delete_font(f: HFONT) {
    if !f.is_invalid() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
    }
}

pub fn set_text(h: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(h, &HSTRING::from(text));
    }
}

pub fn get_text(h: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

/// Window rectangle for a client area of `w`×`h` logical pixels, centered
/// in `area`.
pub fn centered_window_rect(
    area: RECT,
    w: i32,
    h: i32,
    scale: f32,
    style: u32,
    ex: u32,
    dpi: u32,
) -> RECT {
    use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
    let mut r = RECT {
        left: 0,
        top: 0,
        right: (w as f32 * scale) as i32,
        bottom: (h as f32 * scale) as i32,
    };
    unsafe {
        let _ =
            AdjustWindowRectExForDpi(&mut r, WINDOW_STYLE(style), false, WINDOW_EX_STYLE(ex), dpi);
    }
    let (ww, wh) = (r.right - r.left, r.bottom - r.top);
    let x = area.left + ((area.right - area.left) - ww) / 2;
    let y = area.top + ((area.bottom - area.top) - wh) / 2;
    RECT {
        left: x.max(area.left),
        top: y.max(area.top),
        right: x.max(area.left) + ww,
        bottom: y.max(area.top) + wh,
    }
}
