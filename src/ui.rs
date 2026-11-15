//! Shared helpers for Berth's native windows (Dashboard and Add app).

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT, HGDIOBJ};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, SendMessageW, SetWindowTextW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_CHILD, WS_TABSTOP, WS_VISIBLE,
};

/// Raw Win32 constants for common controls (kept numeric to avoid type
/// juggling between the many typed wrappers).
pub mod c {
    pub const BS_PUSHBUTTON: u32 = 0x0;
    pub const BS_AUTOCHECKBOX: u32 = 0x3;
    pub const BM_GETCHECK: u32 = 0xF0;
    pub const BM_SETCHECK: u32 = 0xF1;
    pub const BN_CLICKED: u16 = 0;

    pub const CBS_DROPDOWNLIST: u32 = 0x3;
    pub const CB_ADDSTRING: u32 = 0x143;
    pub const CB_GETCURSEL: u32 = 0x147;
    pub const CB_RESETCONTENT: u32 = 0x14B;
    pub const CB_SETCURSEL: u32 = 0x14E;
    pub const CBN_SELCHANGE: u16 = 1;

    pub const LBS_NOTIFY: u32 = 0x1;
    pub const LBS_OWNERDRAWFIXED: u32 = 0x10;
    pub const LBS_HASSTRINGS: u32 = 0x40;
    pub const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
    pub const LB_ADDSTRING: u32 = 0x180;
    pub const LB_SETCURSEL: u32 = 0x186;
    pub const LB_GETCURSEL: u32 = 0x188;
    pub const LBN_SELCHANGE: u16 = 1;

    pub const TBS_NOTICKS: u32 = 0x10;
    pub const TBM_GETPOS: u32 = 0x400;
    pub const TBM_SETPOS: u32 = 0x405;
    pub const TBM_SETRANGEMIN: u32 = 0x407;
    pub const TBM_SETRANGEMAX: u32 = 0x408;
    pub const TBM_SETPAGESIZE: u32 = 0x415;

    pub const ES_AUTOHSCROLL: u32 = 0x80;
    pub const EM_SETCUEBANNER: u32 = 0x1501;
    pub const EN_CHANGE: u16 = 0x300;

    pub const SS_NOPREFIX: u32 = 0x80;

    pub const LVS_REPORT: u32 = 0x1;
    pub const LVS_SINGLESEL: u32 = 0x4;
    pub const LVS_SHOWSELALWAYS: u32 = 0x8;
    pub const LVS_SHAREIMAGELISTS: u32 = 0x40;
    pub const LVS_EDITLABELS: u32 = 0x200;
    pub const LVS_NOCOLUMNHEADER: u32 = 0x4000;
    pub const LVS_EX_FULLROWSELECT: usize = 0x20;
    pub const LVS_EX_DOUBLEBUFFER: usize = 0x10000;
    pub const LVM_SETIMAGELIST: u32 = 0x1003;
    pub const LVM_DELETEALLITEMS: u32 = 0x1009;
    pub const LVM_GETNEXTITEM: u32 = 0x100C;
    pub const LVM_ENSUREVISIBLE: u32 = 0x1013;
    pub const LVM_SETITEMSTATE: u32 = 0x102B;
    pub const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
    pub const LVM_INSERTITEMW: u32 = 0x104D;
    pub const LVM_INSERTCOLUMNW: u32 = 0x1061;
    pub const LVM_SETITEMTEXTW: u32 = 0x1074;
    pub const LVM_EDITLABELW: u32 = 0x1076;
    pub const LVNI_SELECTED: usize = 0x2;
    pub const LVSIL_SMALL: usize = 1;
    pub const LVIF_TEXT: u32 = 0x1;
    pub const LVIF_IMAGE: u32 = 0x2;
    pub const LVIF_PARAM: u32 = 0x4;
    pub const LVIS_FOCUSED: u32 = 0x1;
    pub const LVIS_SELECTED: u32 = 0x2;
    pub const LVCF_WIDTH: u32 = 0x2;
    pub const LVCF_TEXT: u32 = 0x4;
    pub const I_IMAGECALLBACK: i32 = -1;

    pub const NM_DBLCLK: i32 = -3;
    pub const NM_RETURN: i32 = -4;
    pub const LVN_ITEMCHANGED: i32 = -101;
    pub const LVN_BEGINLABELEDITW: i32 = -175;
    pub const LVN_ENDLABELEDITW: i32 = -176;
    pub const LVN_GETDISPINFOW: i32 = -177;

    pub const IDOK: u16 = 1;
    pub const IDCANCEL: u16 = 2;
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

/// The Windows message font at a given DPI, optionally resized/bolded.
pub fn message_font(dpi: u32, size_factor: f32, weight: i32) -> HFONT {
    unsafe {
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            0,
            dpi,
        );
        let mut lf = ncm.lfMessageFont;
        if lf.lfHeight == 0 {
            lf.lfHeight = -((9.0 * dpi as f32 / 72.0).round() as i32);
        }
        lf.lfHeight = (lf.lfHeight as f32 * size_factor).round() as i32;
        if weight > 0 {
            lf.lfWeight = weight;
        }
        CreateFontIndirectW(&lf)
    }
}

pub fn delete_font(f: HFONT) {
    if !f.is_invalid() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
    }
}

/// Creates a child control. Coordinates are in logical (96-DPI) pixels.
#[allow(clippy::too_many_arguments)]
pub fn child(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    ex: u32,
    r: (i32, i32, i32, i32),
    id: u16,
    font: HFONT,
    scale: f32,
) -> HWND {
    let s = |v: i32| (v as f32 * scale).round() as i32;
    let tab = if class == "STATIC" { 0 } else { WS_TABSTOP.0 };
    unsafe {
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(ex),
            &HSTRING::from(class),
            &HSTRING::from(text),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | tab | style),
            s(r.0),
            s(r.1),
            s(r.2),
            s(r.3),
            Some(parent),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                id as usize as *mut _,
            )),
            None,
            None,
        )
        .unwrap_or_default();
        send(h, WM_SETFONT, font.0 as usize, 1);
        h
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
