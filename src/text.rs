//! Text for hover labels. GDI rasterizes the text in grayscale (no
//! ClearType, which needs an opaque background) and the result is used as
//! an alpha mask, so labels blend cleanly over the translucent dock.

use tiny_skia::{IntSize, Pixmap};
use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CALCRECT,
    DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, FF_SWISS, FW_MEDIUM, HGDIOBJ, OUT_TT_PRECIS,
    TRANSPARENT,
};

/// Renders `text` in the given color; returns a premultiplied pixmap.
pub fn render(text: &str, font_px: i32, max_width: i32, rgb: (u8, u8, u8)) -> Option<Pixmap> {
    if text.is_empty() {
        return None;
    }
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let font = CreateFontW(
            -font_px,
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
            w!("Segoe UI Variable Text"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let mut wtext: Vec<u16> = text.encode_utf16().collect();
        let mut measure = RECT {
            left: 0,
            top: 0,
            right: max_width,
            bottom: font_px * 2,
        };
        DrawTextW(
            dc,
            &mut wtext,
            &mut measure,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
        let w = (measure.right - measure.left).clamp(1, max_width) + 2;
        let h = (measure.bottom - measure.top).max(1) + 2;

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
        let result = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) if !bits.is_null() => {
                let old_bmp = SelectObject(dc, HGDIOBJ(dib.0));
                std::ptr::write_bytes(bits as *mut u8, 0, (w * h * 4) as usize);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00FF_FFFF));
                let mut r = RECT {
                    left: 1,
                    top: 1,
                    right: w - 1,
                    bottom: h - 1,
                };
                DrawTextW(
                    dc,
                    &mut wtext,
                    &mut r,
                    DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
                );
                let src = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                let mut out = vec![0u8; src.len()];
                for (o, s) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    // White text on black: any channel gives coverage.
                    let a = s[0].max(s[1]).max(s[2]) as u16;
                    o[0] = ((rgb.0 as u16 * a + 127) / 255) as u8;
                    o[1] = ((rgb.1 as u16 * a + 127) / 255) as u8;
                    o[2] = ((rgb.2 as u16 * a + 127) / 255) as u8;
                    o[3] = a as u8;
                }
                SelectObject(dc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(dib.0));
                Pixmap::from_vec(out, IntSize::from_wh(w as u32, h as u32)?)
            }
            _ => None,
        };
        SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);
        result
    }
}
