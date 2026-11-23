# Changelog

## 0.1.3 — 2026-11-24

- **New:** optional **Hide the Windows taskbar automatically while Berth is running**
  (Dashboard > General). Berth turns on Windows' own taskbar auto-hide, so the taskbar
  slides back in when the pointer reaches its edge (top, bottom, left or right), and puts
  your previous setting back when you turn the option off, quit Berth, or sign out.
- **New:** update notifications. Berth checks GitHub at start-up and about twice a day
  and shows a notification when a new version is out. On by default; turn it off in
  Dashboard > Updates. Installing still always asks first.
- After an update, a notification says what's new.

## 0.1.2 — 2026-11-15

- **New:** one-click updates. Dashboard > **Updates** > **Check for updates and install**
  finds the latest release on GitHub, downloads it, verifies its SHA-256 checksum,
  replaces `Berth.exe` and restarts Berth. Also available from the tray menu.
- **New:** optional "Check for updates automatically when Berth starts" (off by default).
- Berth now only goes online when checking for updates; the privacy notes were updated.

## 0.1.1 — 2026-11-15

- **Fixed:** left-clicking an icon now opens the app. Previously only the right-click
  menu's **Open** worked, because releasing the mouse cancelled the click.
- **Fixed:** dragging icons along the dock to reorder them now works (same cause).

## 0.1.0 — 2026-11-15

- First release: dock, Berth Dashboard, pinned apps, running indicators, hover effects,
  opt-in launch at sign-in, tray icon, multi-monitor, auto-hide and full-screen hiding.
