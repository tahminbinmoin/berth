//! User preferences and pinned apps, persisted as JSON in
//! `%LOCALAPPDATA%\Berth\settings.json`.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::util;

pub const SETTINGS_VERSION: u32 = 1;
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
    pub auto_hide: bool,
    pub hide_in_fullscreen: bool,
    /// Opt-in: look for a new release on GitHub when Berth starts.
    pub check_updates_on_startup: bool,
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
            auto_hide: false,
            hide_in_fullscreen: true,
            check_updates_on_startup: false,
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
    }

    /// Resets appearance and behavior while keeping the pinned apps.
    pub fn defaults_keeping_items(&self) -> Settings {
        Settings {
            items: self.items.clone(),
            ..Settings::default()
        }
    }
}

/// A fallback label derived from a path: the file name without extension.
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
