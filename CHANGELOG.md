# Changelog

## 0.1.7 — 2026-11-24

- **Fixed:** the dock stayed hidden while a maximized app was in front. Since maximized
  windows can now fill the whole screen (when Berth hides the taskbar), Berth mistook
  them for full-screen apps. Only true full-screen apps (games, videos, presentations)
  hide the dock now.

## 0.1.6 — 2026-11-24

- **Improved:** with the taskbar on the left, top or right, apps now get the full screen
  while it's hidden. Instead of hiding the taskbar window (which made Explorer reserve its
  strip and resize windows), Berth clips it to an invisible, click-through shape and gives
  the strip to apps without the system-wide broadcast that made Explorer take it back.
  Maximized windows are resized to fill the screen, and everything is put back on quit.
- **New:** the taskbar wipes in from the screen edge when revealed (follows the Animations
  setting).

## 0.1.5 — 2026-11-24

- **Fixed:** with the taskbar on the left, top or right and **Hide the Windows taskbar** on,
  the screen kept "jumping". Berth repeatedly asked Windows to give the taskbar's strip to
  apps, Windows took it back, and every round resized all maximized windows. Berth no
  longer changes the screen's work area at all (it also repairs one left over by 0.1.4).
- **Fixed:** if Explorer keeps showing the taskbar again right after Berth hides it, Berth
  now pauses for 30 seconds instead of fighting it.

## 0.1.4 — 2026-11-24

- **Fixed:** **Hide the Windows taskbar** didn't work when the taskbar was on the left, top
  or right. Windows only allows its own auto-hide at the bottom, so for other edges Berth
  now hides the taskbar itself and shows it when the pointer reaches that edge (or while
  Start, search or another shell pop-up is open). Berth also tries to give the taskbar's
  strip to apps while it's hidden (best-effort). Everything is restored on quit, sign-out,
  or when the option is turned off.
  If Berth stops unexpectedly, starting it again shows the taskbar immediately.

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
