//! Adding apps: the "Add apps" panel lives in the dashboard; this module
//! keeps the Windows file/folder picker.

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_ALLOWMULTISELECT,
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NODEREFERENCELINKS, FOS_PICKFOLDERS,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::MSG;

use crate::util;

pub fn register(_hinst: HINSTANCE) -> bool {
    true
}

pub fn dialog_message(_msg: &MSG) -> bool {
    false
}

pub fn close() {}

/// Opens the dashboard's "Add apps" panel.
pub fn open() {
    crate::dashboard::open_add_apps();
}

/// Shows the standard Windows file (or folder) picker. Shortcuts are kept
/// as shortcuts rather than resolved to their targets.
pub fn browse(owner: Option<HWND>, folders: bool) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let Ok(dlg) =
            CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
        else {
            util::error_box(owner, "Windows couldn't open the file picker.");
            return out;
        };
        let mut opts = dlg.GetOptions().unwrap_or_default();
        opts |= FOS_FORCEFILESYSTEM | FOS_ALLOWMULTISELECT;
        if folders {
            opts |= FOS_PICKFOLDERS;
            let _ = dlg.SetTitle(w!("Choose folders to add to the dock"));
        } else {
            opts |= FOS_FILEMUSTEXIST | FOS_NODEREFERENCELINKS;
            let _ = dlg.SetTitle(w!("Choose apps, shortcuts or files to add to the dock"));
            let specs = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Apps and shortcuts"),
                    pszSpec: w!("*.exe;*.lnk;*.url;*.appref-ms;*.bat;*.cmd"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All files"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = dlg.SetFileTypes(&specs);
            if let Ok(start) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from("shell:Common Programs"),
                None,
            ) {
                let _ = dlg.SetFolder(&start);
            }
        }
        let _ = dlg.SetOptions(opts);
        if dlg.Show(owner).is_err() {
            return out; // cancelled
        }
        if let Ok(results) = dlg.GetResults() {
            let n = results.GetCount().unwrap_or(0);
            for i in 0..n {
                if let Ok(item) = results.GetItemAt(i) {
                    if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                        let s = util::take_pwstr(p);
                        if !s.is_empty() {
                            out.push(s);
                        }
                    }
                }
            }
        }
    }
    out
}
