//! Queries about the Windows environment: theme, animation preference,
//! displays, full-screen detection.

use windows::core::{BOOL, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, SystemParametersInfoW,
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::config::{AnimationMode, ThemeMode};
use crate::util::from_wide;

fn read_hkcu_dword(subkey: &str, value: &str) -> Option<u32> {
    let mut data: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(data)
}

/// True when Windows apps use the dark theme.
pub fn system_prefers_dark() -> bool {
    read_hkcu_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .map(|v| v == 0)
    .unwrap_or(false)
}

pub fn is_dark(mode: ThemeMode) -> bool {
    match mode {
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
        ThemeMode::System => system_prefers_dark(),
    }
}

/// Windows "Animation effects" setting.
pub fn system_animations_enabled() -> bool {
    let mut enabled = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut enabled as *mut BOOL as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_err() || enabled.as_bool()
}

pub fn animations_enabled(mode: AnimationMode) -> bool {
    match mode {
        AnimationMode::On => true,
        AnimationMode::Off => false,
        AnimationMode::System => system_animations_enabled(),
    }
}

#[derive(Clone, Debug)]
pub struct Monitor {
    pub handle: HMONITOR,
    /// Device name such as `\\.\DISPLAY1`.
    pub device: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub dpi: u32,
}

impl Monitor {
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Human-readable label for the settings window.
    pub fn label(&self, index: usize) -> String {
        let w = self.rect.right - self.rect.left;
        let h = self.rect.bottom - self.rect.top;
        format!(
            "Display {} \u{2014} {} \u{00d7} {}{}",
            index + 1,
            w,
            h,
            if self.primary { " (main)" } else { "" }
        )
    }
}

unsafe extern "system" fn enum_monitor(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<Monitor>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_err() || dx == 0 {
            dx = 96;
        }
        list.push(Monitor {
            handle: hmon,
            device: from_wide(&info.szDevice),
            rect: info.monitorInfo.rcMonitor,
            work: info.monitorInfo.rcWork,
            primary: info.monitorInfo.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
            dpi: dx,
        });
    }
    BOOL(1)
}

/// All connected displays, primary first, then left-to-right.
pub fn monitors() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    list
}

/// Resolves the configured display, falling back to the primary display if
/// the chosen one is disconnected.
pub fn pick_monitor(pref: &str) -> Option<Monitor> {
    let list = monitors();
    if pref != "primary" {
        if let Some(m) = list.iter().find(|m| m.device.eq_ignore_ascii_case(pref)) {
            return Some(m.clone());
        }
    }
    list.into_iter().next()
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    from_wide(&buf[..n.max(0) as usize])
}

/// True when the foreground window covers the whole given monitor (a game,
/// video or presentation). The desktop itself does not count.
pub fn fullscreen_app_on(monitor: HMONITOR, own: &[HWND]) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() || own.contains(&fg) {
            return false;
        }
        let cls = class_name(fg);
        if matches!(
            cls.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
        let m = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
        if m != monitor {
            return false;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(m, &mut info).as_bool() {
            return false;
        }
        let mut r = RECT::default();
        // A maximized window is not "full screen", even when it fills the
        // whole display (e.g. while Berth hides the taskbar).
        if windows::Win32::UI::WindowsAndMessaging::IsZoomed(fg).as_bool() {
            return false;
        }
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let mr = info.rcMonitor;
        r.left <= mr.left && r.top <= mr.top && r.right >= mr.right && r.bottom >= mr.bottom
    }
}
