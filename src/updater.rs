//! One-click updates from this project's GitHub releases.
//!
//! Berth only goes online when the user presses "Check for updates" (or has
//! turned on the opt-in startup check). It then:
//! 1. asks the GitHub API for the latest release of `tahminbinmoin/berth`;
//! 2. if it is newer, downloads the release zip and its `.sha256` file from
//!    that repository's release downloads only;
//! 3. verifies the SHA-256 checksum, extracts `Berth.exe` and checks that
//!    it is a Windows program;
//! 4. renames the running `Berth.exe` to `Berth.exe.old`, puts the new one in
//!    its place, starts it and exits. If anything fails, the old file is put
//!    back and nothing changes.
//!
//! Network work runs on a background thread; results are posted to the
//! controller window and handled on the UI thread.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::util;

pub const OWNER: &str = "tahminbinmoin";
pub const REPO: &str = "berth";
const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub zip_url: String,
    pub sha_url: String,
}

/// Results handed from the worker thread to the UI thread.
pub enum Outcome {
    Checked {
        silent: bool,
        result: Result<Option<Release>, String>,
    },
    Prepared(Result<(Release, PathBuf), String>),
}

static OUTCOME: Mutex<Option<Outcome>> = Mutex::new(None);
static BUSY: Mutex<bool> = Mutex::new(false);

pub fn take_outcome() -> Option<Outcome> {
    OUTCOME.lock().ok()?.take()
}

pub fn is_busy() -> bool {
    BUSY.lock().map(|b| *b).unwrap_or(false)
}

fn set_busy(v: bool) {
    if let Ok(mut b) = BUSY.lock() {
        *b = v;
    }
}

fn post(notify: isize, msg: u32, outcome: Outcome) {
    if let Ok(mut o) = OUTCOME.lock() {
        *o = Some(outcome);
    }
    set_busy(false);
    unsafe {
        let _ = PostMessageW(Some(HWND(notify as *mut _)), msg, WPARAM(0), LPARAM(0));
    }
}

// ----- versions --------------------------------------------------------------

pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u32>().ok());
    Some((
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    ))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ----- HTTPS (WinHTTP) --------------------------------------------------------

struct Handle(*mut core::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// HTTPS GET. Only `https://` URLs; redirects are followed by WinHTTP.
fn http_get(url: &str, accept: &str) -> Result<Vec<u8>, String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or("only HTTPS addresses are allowed")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let agent = HSTRING::from(format!(
        "Berth/{} (+https://github.com/{OWNER}/{REPO})",
        crate::VERSION
    ));
    unsafe {
        let session = Handle(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(format!(
                "network is unavailable ({})",
                windows::core::Error::from_thread()
            ));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let conn = Handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0));
        if conn.0.is_null() {
            return Err(format!(
                "couldn't reach {host} ({})",
                windows::core::Error::from_thread()
            ));
        }
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err(format!(
                "couldn't create the request ({})",
                windows::core::Error::from_thread()
            ));
        }
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        let _ = WinHttpAddRequestHeaders(req.0, &headers, WINHTTP_ADDREQ_FLAG_ADD);
        WinHttpSendRequest(req.0, None, None, 0, 0, 0)
            .map_err(|e| format!("couldn't connect to {host}: {}", e.message()))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut())
            .map_err(|e| format!("no response from {host}: {}", e.message()))?;

        let mut status: u32 = 0;
        let mut len = 4u32;
        let mut index = 0u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            &mut index,
        )
        .map_err(|e| e.message().to_string())?;
        if status == 404 {
            return Err("no published release was found".into());
        }
        if status != 200 {
            return Err(format!("GitHub answered with HTTP status {status}"));
        }

        let mut body = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail)
                .map_err(|e| format!("download interrupted: {}", e.message()))?;
            if avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(
                req.0,
                body[start..].as_mut_ptr() as *mut _,
                avail,
                &mut read,
            )
            .map_err(|e| format!("download interrupted: {}", e.message()))?;
            body.truncate(start + read as usize);
            if body.len() > MAX_DOWNLOAD {
                return Err("the download is unexpectedly large".into());
            }
        }
        Ok(body)
    }
}

// ----- release lookup ------------------------------------------------------------

fn latest_release() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let body = http_get(&url, "application/vnd.github+json")?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| "GitHub sent an unexpected reply")?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or("the latest release has no version tag")?;
    let allowed = format!("https://github.com/{OWNER}/{REPO}/releases/download/");
    let mut zip_url = None;
    let mut sha_url = None;
    for a in json["assets"].as_array().into_iter().flatten() {
        let name = a["name"].as_str().unwrap_or_default();
        let url = a["browser_download_url"].as_str().unwrap_or_default();
        if !url.starts_with(&allowed) {
            continue;
        }
        if name.ends_with("-windows-x64.zip") {
            zip_url = Some(url.to_string());
        } else if name.ends_with("-windows-x64.zip.sha256") {
            sha_url = Some(url.to_string());
        }
    }
    Ok(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        zip_url: zip_url.ok_or("the latest release has no Windows download")?,
        sha_url: sha_url.ok_or("the latest release has no checksum file")?,
    })
}

/// Starts a background check. `silent` checks only report when an update
/// is available. `notify` receives `msg` when done.
pub fn start_check(notify: HWND, msg: u32, silent: bool) -> bool {
    if is_busy() {
        return false;
    }
    set_busy(true);
    let h = notify.0 as isize;
    std::thread::spawn(move || {
        let result = latest_release().map(|r| is_newer(&r.version, crate::VERSION).then_some(r));
        if let Err(e) = &result {
            util::log(&format!("update check failed: {e}"));
        }
        post(h, msg, Outcome::Checked { silent, result });
    });
    true
}

// ----- download and verify ---------------------------------------------------------

fn sha256_hex(data: &[u8]) -> Result<String, String> {
    let mut out = [0u8; 32];
    let st = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    if st.is_err() {
        return Err("Windows couldn't compute the checksum".into());
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

fn u16le(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}
fn u32le(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// Extracts `Berth.exe` from a zip archive (stored or deflated entries).
pub fn extract_exe(zip: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "the download isn't a valid Berth package".to_string();
    // End-of-central-directory record.
    let eocd = (0..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or_else(bad)?;
    let count = u16le(zip, eocd + 10).ok_or_else(bad)?;
    let mut p = u32le(zip, eocd + 16).ok_or_else(bad)?;
    for _ in 0..count {
        if !zip
            .get(p..)
            .ok_or_else(bad)?
            .starts_with(&[0x50, 0x4b, 0x01, 0x02])
        {
            return Err(bad());
        }
        let method = u16le(zip, p + 10).ok_or_else(bad)?;
        let csize = u32le(zip, p + 20).ok_or_else(bad)?;
        let usize_ = u32le(zip, p + 24).ok_or_else(bad)?;
        let nlen = u16le(zip, p + 28).ok_or_else(bad)?;
        let elen = u16le(zip, p + 30).ok_or_else(bad)?;
        let clen = u16le(zip, p + 32).ok_or_else(bad)?;
        let local = u32le(zip, p + 42).ok_or_else(bad)?;
        let name =
            String::from_utf8_lossy(zip.get(p + 46..p + 46 + nlen).ok_or_else(bad)?).to_string();
        p += 46 + nlen + elen + clen;
        let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
        if !base.eq_ignore_ascii_case("Berth.exe") {
            continue;
        }
        if !zip
            .get(local..)
            .ok_or_else(bad)?
            .starts_with(&[0x50, 0x4b, 0x03, 0x04])
        {
            return Err(bad());
        }
        let ln = u16le(zip, local + 26).ok_or_else(bad)?;
        let le = u16le(zip, local + 28).ok_or_else(bad)?;
        let start = local + 30 + ln + le;
        let data = zip.get(start..start + csize).ok_or_else(bad)?;
        let out = match method {
            0 => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, MAX_DOWNLOAD)
                .map_err(|_| bad())?,
            _ => return Err("the package uses an unsupported compression method".into()),
        };
        if out.len() != usize_ || !out.starts_with(b"MZ") {
            return Err(bad());
        }
        return Ok(out);
    }
    Err("Berth.exe wasn't found in the download".into())
}

fn update_dir() -> PathBuf {
    let d = util::data_dir().join("update");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Downloads, verifies and unpacks a release on a background thread.
pub fn start_download(release: Release, notify: HWND, msg: u32) -> bool {
    if is_busy() {
        return false;
    }
    set_busy(true);
    let h = notify.0 as isize;
    std::thread::spawn(move || {
        let result = (|| {
            let zip = http_get(&release.zip_url, "application/octet-stream")?;
            let sha_text = http_get(&release.sha_url, "application/octet-stream")?;
            let expected = String::from_utf8_lossy(&sha_text)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let actual = sha256_hex(&zip)?;
            if expected.len() != 64 || expected != actual {
                return Err("the download didn't match its checksum, so it was discarded. Please try again later.".to_string());
            }
            let exe = extract_exe(&zip)?;
            let path = update_dir().join(format!("Berth-{}.exe", release.version));
            std::fs::write(&path, &exe).map_err(|e| format!("couldn't save the update: {e}"))?;
            util::log(&format!(
                "update {} downloaded and verified (sha256 {actual})",
                release.version
            ));
            Ok((release, path))
        })();
        if let Err(e) = &result {
            util::log(&format!("update download failed: {e}"));
        }
        post(h, msg, Outcome::Prepared(result));
    });
    true
}

// ----- install -------------------------------------------------------------------

fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// Swaps the running executable for `new_exe`. Windows allows renaming a
/// running program, so the old file is moved aside rather than deleted.
pub fn install(new_exe: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| {
        format!(
            "Berth can't replace its own file in {}.\n({e})\n\nIf Berth is in a protected folder such as Program Files, \
             move it to a folder in your user profile, or update it manually from the Releases page.",
            exe.parent().map(|p| p.display().to_string()).unwrap_or_default()
        )
    })?;
    if let Err(e) = std::fs::copy(new_exe, &exe) {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(format!(
            "couldn't put the new version in place ({e}). Nothing was changed."
        ));
    }
    let _ = std::fs::remove_file(new_exe);
    Ok(exe)
}

/// Undo an install whose restart failed.
pub fn rollback(exe: &Path) {
    let old = old_path(exe);
    if old.exists() {
        let _ = std::fs::remove_file(exe);
        let _ = std::fs::rename(&old, exe);
    }
}

/// Called at startup with `--after-update <pid>`: wait for the previous
/// instance to exit so the single-instance check passes.
pub fn wait_for_previous(pid: u32) {
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, 15_000);
            let _ = CloseHandle(h);
        }
    }
}

/// Removes leftovers from a previous update (best effort).
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let old = old_path(&exe);
        for _ in 0..10 {
            if !old.exists() || std::fs::remove_file(&old).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    if let Ok(entries) = std::fs::read_dir(update_dir()) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(!is_newer("garbage", "0.1.1"));
    }

    #[test]
    fn rejects_non_zip() {
        assert!(extract_exe(b"not a zip at all, definitely not").is_err());
    }
}
