//! Optional "hide the Windows taskbar while Berth is running".
//!
//! This turns on Windows' own taskbar auto-hide (the same switch as
//! Settings > Personalization > Taskbar > Taskbar behaviors > Automatically
//! hide the taskbar) through the documented `SHAppBarMessage(ABM_SETSTATE)`
//! API. Windows then slides the taskbar in whenever the pointer reaches its
//! edge — top, bottom, left or right — on every display.
//!
//! Berth remembers the user's previous setting (in settings.json, so it
//! survives a crash) and puts it back when the option is turned off or Berth
//! quits. Berth never hides, moves or modifies the taskbar window itself.

use windows::core::w;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

use crate::util;

fn data() -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: unsafe { FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default() },
        ..Default::default()
    }
}

/// The taskbar's current state bits, or `None` if no taskbar is running.
fn state() -> Option<u32> {
    let mut d = data();
    if d.hWnd.0.is_null() {
        return None;
    }
    Some(unsafe { SHAppBarMessage(ABM_GETSTATE, &mut d) } as u32)
}

/// Whether Windows' taskbar auto-hide is currently on.
pub fn is_autohide() -> Option<bool> {
    state().map(|s| s & ABS_AUTOHIDE != 0)
}

fn set_bits(bits: u32) -> bool {
    let mut d = data();
    if d.hWnd.0.is_null() {
        return false;
    }
    d.lParam = LPARAM(bits as isize);
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut d) };
    true
}

/// Turns auto-hide on or off, keeping the taskbar's other state bits.
pub fn set_autohide(on: bool) -> bool {
    let Some(cur) = state() else {
        util::log("taskbar not found; auto-hide not changed");
        return false;
    };
    let new = if on {
        cur | ABS_AUTOHIDE
    } else {
        cur & !ABS_AUTOHIDE
    };
    if new == cur {
        return true;
    }
    let ok = set_bits(new);
    util::log(&format!(
        "taskbar auto-hide {}",
        if on { "turned on" } else { "restored to off" }
    ));
    ok
}
