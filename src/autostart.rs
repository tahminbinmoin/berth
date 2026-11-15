//! Opt-in "launch at sign-in" using the per-user Run key:
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `Berth`.
//!
//! Nothing here needs administrator rights. Berth never turns this on by
//! itself, and it never overrides a choice made in Windows Settings >
//! Apps > Startup (that choice lives in the `StartupApproved` key, which
//! Berth only reads).

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};

use crate::util::{self, from_wide};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE: &str = "Berth";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Off,
    On,
    /// Registered, but turned off in Windows Settings > Apps > Startup.
    DisabledByWindows,
}

fn command_line() -> String {
    format!("\"{}\" --startup", util::exe_path())
}

fn read_run_value() -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(VALUE),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok().then(|| from_wide(&buf))
}

fn disabled_in_windows_settings() -> bool {
    let mut buf = [0u8; 16];
    let mut size = buf.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(APPROVED_KEY),
            &HSTRING::from(VALUE),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    // The first byte is 0x02 (enabled) or 0x03 (disabled by the user).
    r.is_ok() && size > 0 && buf[0] & 1 == 1
}

pub fn state() -> State {
    match read_run_value() {
        None => State::Off,
        Some(_) if disabled_in_windows_settings() => State::DisabledByWindows,
        Some(_) => State::On,
    }
}

pub fn enable() -> Result<(), String> {
    let cmd = util::wide(&command_line());
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(VALUE),
            REG_SZ.0,
            Some(cmd.as_ptr() as *const _),
            (cmd.len() * 2) as u32,
        )
    };
    if r.is_ok() {
        util::log("launch at sign-in enabled");
        Ok(())
    } else {
        Err(format!("Windows returned error {}.", r.0))
    }
}

pub fn disable() -> Result<(), String> {
    let r = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(VALUE),
        )
    };
    if r.is_ok() || r == ERROR_FILE_NOT_FOUND {
        util::log("launch at sign-in disabled");
        Ok(())
    } else {
        Err(format!("Windows returned error {}.", r.0))
    }
}

/// If the user opted in earlier and then moved Berth.exe, point the Run entry
/// at the new location. Does nothing when launch at sign-in is off.
pub fn refresh_path_if_enabled() {
    if let Some(current) = read_run_value() {
        if current != command_line() {
            let _ = enable();
        }
    }
}
