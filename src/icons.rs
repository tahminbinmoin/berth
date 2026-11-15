//! Icon loading through the Windows Shell (`IShellItemImageFactory`), which
//! returns the same high-quality icons Explorer and Start use, for files,
//! shortcuts, folders and packaged (Store) apps alike.

use std::collections::HashMap;

use tiny_skia::{IntSize, Pixmap};
use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
};

/// Converts a 32-bit HBITMAP from the shell into a premultiplied RGBA pixmap.
pub unsafe fn hbitmap_to_pixmap(hbm: HBITMAP, w: i32, h: i32) -> Option<Pixmap> {
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: -h, // top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let dc = GetDC(None);
    let lines = GetDIBits(
        dc,
        hbm,
        0,
        h as u32,
        Some(buf.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(None, dc);
    if lines == 0 {
        return None;
    }

    // The shell normally hands back premultiplied alpha, but some icons come
    // back straight (color > alpha) or without any alpha at all.
    let mut any_alpha = false;
    let mut straight = false;
    if buf.iter().all(|&b| b == 0) {
        // An empty bitmap means the shell had no real icon to give.
        return None;
    }
    for px in buf.chunks_exact(4) {
        let a = px[3];
        if a != 0 {
            any_alpha = true;
        }
        if px[0] > a || px[1] > a || px[2] > a {
            straight = true;
        }
    }
    for px in buf.chunks_exact_mut(4) {
        let (b, g, r) = (px[0], px[1], px[2]);
        let mut a = px[3];
        if !any_alpha {
            a = 255;
        }
        let (r, g, b) = if straight && any_alpha {
            let m = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
            (m(r), m(g), m(b))
        } else {
            (r, g, b)
        };
        px[0] = r;
        px[1] = g;
        px[2] = b;
        px[3] = a;
    }
    Pixmap::from_vec(buf, IntSize::from_wh(w as u32, h as u32)?)
}

/// Loads the shell icon for a path or shell location at exactly `px` pixels.
pub fn load_shell_icon(target: &str, px: u32) -> Option<Pixmap> {
    from_image_factory(target, px).or_else(|| from_system_image_list(target, px))
}

fn from_image_factory(target: &str, px: u32) -> Option<Pixmap> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(target), None).ok()?;
        let size = SIZE {
            cx: px as i32,
            cy: px as i32,
        };
        let hbm = factory.GetImage(size, SIIGBF_ICONONLY).ok()?;
        let pm = hbitmap_to_pixmap(hbm, px as i32, px as i32);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        pm
    }
}

/// Fallback: the classic system image list (48 or 256 px), scaled.
fn from_system_image_list(target: &str, px: u32) -> Option<Pixmap> {
    use windows::Win32::UI::Controls::IImageList;
    use windows::Win32::UI::Shell::SHGetImageList;
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    let index = crate::apps::system_icon_index_large(target);
    if index < 0 {
        return None;
    }
    unsafe {
        // SHIL_EXTRALARGE = 2 (48 px), SHIL_JUMBO = 4 (256 px)
        let list: IImageList = SHGetImageList(if px > 64 { 4 } else { 2 }).ok()?;
        let icon = list.GetIcon(index, 0x1 /* ILD_TRANSPARENT */).ok()?;
        let pm = hicon_to_pixmap(icon);
        let _ = DestroyIcon(icon);
        let pm = pm?;
        scale_to(&pm, px)
    }
}

unsafe fn hicon_to_pixmap(icon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<Pixmap> {
    use windows::Win32::Graphics::Gdi::{GetObjectW, BITMAP};
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};
    let mut ii = ICONINFO::default();
    GetIconInfo(icon, &mut ii).ok()?;
    let mut bm = BITMAP::default();
    let have = !ii.hbmColor.is_invalid()
        && GetObjectW(
            HGDIOBJ(ii.hbmColor.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        ) > 0;
    let result = if have {
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let color = hbitmap_to_pixmap(ii.hbmColor, w, h);
        let mask = if ii.hbmMask.is_invalid() {
            None
        } else {
            hbitmap_to_pixmap(ii.hbmMask, w, h)
        };
        color.map(|mut c| {
            // Icons without an alpha channel come back fully opaque; cut out
            // the transparent area using the icon mask (white = transparent).
            let opaque = c.data().chunks_exact(4).all(|p| p[3] == 255);
            if opaque {
                if let Some(m) = mask {
                    for (px, mk) in c
                        .data_mut()
                        .chunks_exact_mut(4)
                        .zip(m.data().chunks_exact(4))
                    {
                        if mk[0] > 127 {
                            px.copy_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                }
            }
            c
        })
    } else {
        None
    };
    if !ii.hbmColor.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
    }
    if !ii.hbmMask.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
    }
    result
}

fn scale_to(src: &Pixmap, px: u32) -> Option<Pixmap> {
    use tiny_skia::{FilterQuality, PixmapPaint, Transform};
    if src.width() == px && src.height() == px {
        return Some(src.clone());
    }
    let mut out = Pixmap::new(px, px)?;
    let k = px as f32 / src.width().max(src.height()) as f32;
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..PixmapPaint::default()
    };
    out.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &paint,
        Transform::from_scale(k, k),
        None,
    );
    Some(out)
}

/// Caches icons by (target, pixel size).
#[derive(Default)]
pub struct IconCache {
    map: HashMap<(String, u32), Option<Pixmap>>,
}

impl IconCache {
    pub fn get(&mut self, target: &str, px: u32) -> Option<&Pixmap> {
        let key = (target.to_ascii_lowercase(), px);
        self.map
            .entry(key)
            .or_insert_with(|| load_shell_icon(target, px))
            .as_ref()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Drops entries for targets that are no longer shown.
    pub fn retain_targets(&mut self, keep: &[String]) {
        let keep: Vec<String> = keep.iter().map(|t| t.to_ascii_lowercase()).collect();
        self.map.retain(|(t, _), _| keep.contains(t));
    }
}
