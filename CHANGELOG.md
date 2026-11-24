# Changelog

## 0.6.0 — 2026-11-24

- **New: Mac-style window animations** (optional, Dashboard > Behavior):
  - minimizing a window shrinks it into its app's dock icon along a curve;
  - clicking the icon of a minimized app grows the window back out of the icon;
  - opening an app from the dock zooms out of its icon to the new window.
  While on, Windows' own minimize/maximize animation is switched off for the current
  sign-in only, and switched back on when the option is turned off or Berth quits.
- New installs get it in the Recommended setup panel.
- Window previews now show minimized windows too, not just the app's icon.
- CI checks that Windows draws live window thumbnails on Berth's overlay.

## 0.5.0 — 2026-11-24

- **New: Mac-style menu bar** (optional) along the top of the dock's display:
  - a Berth menu with Windows Settings, Task Manager, Sleep, Restart, Shut Down, Lock and
    Sign Out (restarting, shutting down and signing out ask first);
  - the name of the app in front, with Hide, Hide Others, Show All and Quit;
  - a Window menu with Minimize, Zoom, Close Window and Bring All to Front;
  - search, the input language (with more than one keyboard layout), Wi-Fi, volume and
    battery (opens Quick Settings), and the date and time (opens notifications).
  It reserves its strip like the taskbar does, so maximized windows stay below it, and it
  steps aside for full-screen apps. Turn it on in Dashboard > Appearance > Menu bar; new
  installs get it in the Recommended setup.
- Berth's taskbar hider keeps the menu bar's strip reserved.
- CI checks that Windows reserves the strip and gives it back.

## 0.4.0 — 2026-11-24

- **New: window previews.** Rest the pointer on an open app to see a live thumbnail of each
  of its windows, with its title. Click one to switch to it; the close button (or a
  middle-click) closes it. Turn previews off in Dashboard > Behavior.
- **New: drag an icon off the dock to remove it.** A "Remove" tag appears once it's far
  enough away.
- **New: the Recycle Bin stays at the end of the dock**, after a divider, like on a Mac.
  Drop files on it to move them to the Recycle Bin. Its icon shows whether it's empty or full.
- **New: icons bounce when an app needs attention** (when its window flashes on the
  taskbar), until you switch to it, with an orange dot.
- **New: start with your taskbar apps.** A new dock starts with the apps pinned to the
  Windows taskbar; Dashboard > Apps > **Import from taskbar** adds them at any time.
- **New look:** a rounder, glassier dock with a bright rim and a softer shadow.
- The dock notices windows opening and closing straight away.
- CI now also tests reordering, dragging off, clicking, attention bouncing and live thumbnails.

## 0.3.0 — 2026-11-24

- **New: Recommended setup.** On first launch the dashboard opens a panel that sets Windows
  up for the dock in one click: start Berth with Windows, move the taskbar to the left,
  hide it until needed, take This PC and Recycle Bin off the desktop, and get update
  notifications. Everything is pre-ticked, nothing changes until you click **Apply**, and
  you can reopen it any time from Dashboard > General.
- **New: Dashboard > General > Windows** card: taskbar position (Bottom, Left, Top, Right),
  hide the taskbar, and show or hide This PC and Recycle Bin on the desktop.
- Moving the taskbar restarts Explorer for a few seconds (in the background; the dashboard
  stays responsive) and checks that Windows really moved it. On Windows versions that only
  allow a bottom taskbar, Berth puts the setting back and says so.
- **Undo setup changes** puts the taskbar position and desktop icons back exactly as they
  were before Berth changed them.
- New installs get **This PC** on the dock next to File Explorer.
- CI now also tests the desktop-icon switch and moving the taskbar (and back) on Windows.

## 0.2.0 — 2026-11-24

- **New: redesigned Berth Dashboard** in the style of Windows 11 Settings: sidebar with
  Fluent icons, rounded setting cards, toggle switches, modern sliders and dropdowns, light
  and dark themes that follow Berth's theme setting, and the Windows accent colour. The
  title bar matches the theme.
- **New:** the **Add apps** panel is built into the dashboard (Apps page), with app icons,
  search and multi-select. The Apps page shows each pinned app's icon, with buttons to
  reorder, rename (inline) and remove.
- Keyboard: Tab moves between controls (with focus rings), Space/Enter activate, arrow
  keys change sliders and dropdowns, Esc closes.
- An automated test now drives the dashboard on Windows in CI (navigation, toggles,
  dropdowns by keyboard and mouse, slider dragging).

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
