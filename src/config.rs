//! User preferences and pinned apps, persisted as JSON in
//! `%LOCALAPPDATA%\Berth\settings.json`.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::util;

pub const SETTINGS_VERSION: u32 = 2;

/// Set when settings from before 0.1.3 were upgraded (update notifications
/// were switched on, and the user is told so once).
pub static MIGRATED_TO_V2: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub const MAX_ITEMS: usize = 64;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Bottom,
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum HoverEffect {
    Magnify,
    Lift,
    Highlight,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum AnimationMode {
    /// Follow Windows Settings > Accessibility > Visual effects > Animation effects.
    System,
    On,
    Off,
}

/// One app, file, folder or shell location pinned to the dock.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct PinnedItem {
    /// Label shown on hover.
    pub name: String,
    /// What to open: a file-system path (`C:\...\app.exe`, a `.lnk`, a folder)
    /// or a shell location such as `shell:AppsFolder\<AppUserModelID>`.
    pub target: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// `"primary"` or a display device name such as `\\.\DISPLAY2`.
    pub display: String,
    pub position: Edge,
    /// Icon size in logical (96-DPI) pixels.
    pub icon_size: u32,
    /// Gap between icons in logical pixels.
    pub spacing: u32,
    /// Distance between the dock and the screen edge in logical pixels.
    pub edge_margin: u32,
    pub hover_effect: HoverEffect,
    /// Maximum magnification in percent (Magnify effect only).
    pub magnification: u32,
    pub theme: ThemeMode,
    /// Background opacity in percent.
    pub opacity: u32,
    pub animations: AnimationMode,
    pub show_running_indicators: bool,
    pub show_running_apps: bool,
    /// Live previews of an app's windows when the pointer rests on its icon.
    pub window_previews: bool,
    /// Keep the Recycle Bin at the end of the dock, after a divider.
    pub trash_at_end: bool,
    /// Show the Mac-style menu bar along the top of the dock's display.
    pub menu_bar: bool,
    /// Mac-style minimize/restore/launch animations into and out of the dock.
    pub window_animations: bool,
    /// Optional features Berth has already offered after an update.
    pub offered_features: Vec<String>,
    pub auto_hide: bool,
    pub hide_in_fullscreen: bool,
    /// Look for a new release on GitHub at start-up and twice a day, and
    /// show a notification when one is available. Installing always asks.
    pub check_updates_on_startup: bool,
    /// Turn on Windows' taskbar auto-hide while Berth is running.
    pub hide_taskbar: bool,
    /// The taskbar auto-hide state before Berth changed it; restored when the
    /// option is turned off or Berth quits. `None` = Berth hasn't changed it.
    pub taskbar_autohide_before: Option<bool>,
    /// True while Berth itself has the taskbar hidden (left/top/right
    /// taskbars); used to show it again after an unexpected exit.
    pub taskbar_hidden_by_berth: bool,
    /// Taskbar edge code stored by Windows before Berth's setup moved it
    /// (for "Undo setup changes").
    pub taskbar_edge_before: Option<u8>,
    /// Desktop icon values (This PC, Recycle Bin) before Berth changed them.
    pub desktop_icons_before: Option<[Option<u32>; 2]>,
    pub items: Vec<PinnedItem>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            display: "primary".into(),
            position: Edge::Bottom,
            icon_size: 48,
            spacing: 8,
            edge_margin: 8,
            hover_effect: HoverEffect::Magnify,
            magnification: 150,
            theme: ThemeMode::System,
            opacity: 72,
            animations: AnimationMode::System,
            show_running_indicators: true,
            show_running_apps: false,
            window_previews: true,
            trash_at_end: true,
            menu_bar: false,
            window_animations: false,
            offered_features: Vec::new(),
            auto_hide: false,
            hide_in_fullscreen: true,
            check_updates_on_startup: true,
            hide_taskbar: false,
            taskbar_autohide_before: None,
            taskbar_hidden_by_berth: false,
            taskbar_edge_before: None,
            desktop_icons_before: None,
            items: Vec::new(),
        }
    }
}

impl Settings {
    /// Clamps every value into its supported range so a hand-edited file can
    /// never produce an unusable dock.
    pub fn sanitize(&mut self) {
        self.version = SETTINGS_VERSION;
        self.icon_size = self.icon_size.clamp(24, 128);
        self.spacing = self.spacing.min(32);
        self.edge_margin = self.edge_margin.min(64);
        self.magnification = self.magnification.clamp(100, 200);
        self.opacity = self.opacity.clamp(20, 100);
        if self.display.trim().is_empty() {
            self.display = "primary".into();
        }
        self.items.retain(|i| !i.target.trim().is_empty());
        for i in &mut self.items {
            i.target = i.target.trim().to_string();
            if i.name.trim().is_empty() {
                i.name = display_name_from_target(&i.target);
            }
        }
        self.items.truncate(MAX_ITEMS);
        if self.trash_at_end {
            if let Some(i) = self.items.iter().position(|it| is_recycle_bin(&it.target)) {
                let bin = self.items.remove(i);
                self.items.push(bin);
            }
        }
    }

    /// Resets appearance and behavior while keeping the pinned apps.
    pub fn defaults_keeping_items(&self) -> Settings {
        Settings {
            items: self.items.clone(),
            taskbar_autohide_before: self.taskbar_autohide_before,
            taskbar_hidden_by_berth: self.taskbar_hidden_by_berth,
            taskbar_edge_before: self.taskbar_edge_before,
            offered_features: self.offered_features.clone(),
            desktop_icons_before: self.desktop_icons_before,
            ..Settings::default()
        }
    }
}

/// A fallback label derived from a path: the file name without extension.
/// The shell location of the Recycle Bin.
pub const RECYCLE_BIN: &str = "shell:RecycleBinFolder";

pub fn is_recycle_bin(target: &str) -> bool {
    target.trim().eq_ignore_ascii_case(RECYCLE_BIN)
}

pub fn display_name_from_target(target: &str) -> String {
    let t = target.trim_end_matches(['\\', '/']);
    let last = t.rsplit(['\\', '/', '!']).next().unwrap_or(t);
    let stem = match last.rfind('.') {
        Some(i) if i > 0 => &last[..i],
        _ => last,
    };
    if stem.is_empty() {
        target.to_string()
    } else {
        stem.to_string()
    }
}

pub fn settings_path() -> PathBuf {
    util::data_dir().join("settings.json")
}

pub enum LoadOutcome {
    /// No settings file yet (first run).
    FirstRun(Settings),
    Loaded(Settings),
    /// The file existed but could not be read; it was moved aside.
    Recovered {
        settings: Settings,
        backup: PathBuf,
        reason: String,
    },
}

pub fn load() -> LoadOutcome {
    let path = settings_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LoadOutcome::FirstRun(Settings::default());
        }
        Err(e) => {
            return recover(&path, format!("the file could not be opened ({e})"));
        }
    };
    match serde_json::from_str::<Settings>(&text) {
        Ok(mut s) => {
            if s.version < 2 {
                // 0.1.3: update notifications are on by default.
                s.check_updates_on_startup = true;
                MIGRATED_TO_V2.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            s.sanitize();
            LoadOutcome::Loaded(s)
        }
        Err(e) => recover(&path, format!("the file is not valid ({e})")),
    }
}

fn recover(path: &PathBuf, reason: String) -> LoadOutcome {
    let backup = path.with_file_name(format!("settings.broken-{}.json", unix_time()));
    let _ = fs::rename(path, &backup);
    util::log(&format!(
        "settings could not be loaded: {reason}; moved to {}",
        backup.display()
    ));
    LoadOutcome::Recovered {
        settings: Settings::default(),
        backup,
        reason,
    }
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Writes settings atomically (temp file + rename).
pub fn save(settings: &Settings) -> Result<(), String> {
    let path = settings_path();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&tmp, json).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("could not replace {}: {e}", path.display()))?;
    Ok(())
}

/// Moves the current settings file aside (used by `--reset-settings`).
pub fn reset_file() -> Option<PathBuf> {
    let path = settings_path();
    if path.exists() {
        let backup = path.with_file_name(format!("settings.before-reset-{}.json", unix_time()));
        if fs::rename(&path, &backup).is_ok() {
            return Some(backup);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_values() {
        let mut s = Settings {
            icon_size: 5,
            opacity: 500,
            magnification: 999,
            ..Settings::default()
        };
        s.items.push(PinnedItem {
            name: "".into(),
            target: r"C:\Tools\thing.exe".into(),
        });
        s.items.push(PinnedItem {
            name: "x".into(),
            target: "   ".into(),
        });
        s.sanitize();
        assert_eq!(s.icon_size, 24);
        assert_eq!(s.opacity, 100);
        assert_eq!(s.magnification, 200);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].name, "thing");
    }

    #[test]
    fn partial_json_uses_defaults() {
        let s: Settings = serde_json::from_str(r#"{"position":"left"}"#).unwrap();
        assert_eq!(s.position, Edge::Left);
        assert_eq!(s.icon_size, 48);
    }

    #[test]
    fn restore_defaults_keeps_taskbar_memory() {
        let s = Settings {
            hide_taskbar: true,
            taskbar_autohide_before: Some(false),
            ..Settings::default()
        };
        let d = s.defaults_keeping_items();
        assert!(!d.hide_taskbar);
        assert_eq!(d.taskbar_autohide_before, Some(false));
    }

    #[test]
    fn names_from_targets() {
        assert_eq!(display_name_from_target(r"C:\a\Notepad++.lnk"), "Notepad++");
        assert_eq!(
            display_name_from_target(
                r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
            ),
            "App"
        );
        assert_eq!(display_name_from_target(r"D:\Projects\"), "Projects");
    }
}
