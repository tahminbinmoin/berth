# Contributing to Berth

Thanks for helping! Berth aims to stay **small, fast, offline and honest**:

- No accounts, telemetry, ads or cloud services. The only network access is the
  user-initiated (or opt-in) update check against this repository's GitHub releases.
- No administrator rights, no changes to Windows system files, and no changes to taskbar
  settings.
- Use only documented Windows APIs. If a feature can only be done with undocumented
  tricks, leave it out or put it behind a clearly labelled fallback.
- Don't claim support for anything that hasn't been tested on Windows 11.

## Reporting bugs

Open an issue with your Windows version (**Settings > System > About**), display setup
(number of monitors and scaling), what you expected and what happened. Attach the
relevant part of `%LOCALAPPDATA%\Berth\berth.log` after checking it for private paths.

## Code layout

| File | Purpose |
|---|---|
| `src/main.rs` | Start-up, single instance, hidden controller window, menus, actions, message loop |
| `src/dock.rs` | The dock window: geometry, layout, hit-testing, animation, drawing |
| `src/dashboard.rs` | The Berth Dashboard window (all settings) |
| `src/picker.rs` | "Add apps" window and the file/folder picker |
| `src/apps.rs` | Launching, window enumeration and matching, installed-app list |
| `src/icons.rs` | Shell icon loading and caching |
| `src/config.rs` | Settings model, validation, load/save |
| `src/autostart.rs` | Opt-in launch at sign-in (HKCU Run key) |
| `src/updater.rs` | Update check, download, checksum verification and self-replacement |
| `src/system.rs` | Theme, animation preference, monitors, full-screen detection |
| `src/tray.rs` | Notification-area icon |
| `src/draw.rs`, `src/text.rs` | Drawing helpers (tiny-skia) and label text rendering |
| `src/ui.rs` | Shared helpers for the native windows |
| `assets/` | App icon, manifest and resource script (embedded by `build.rs`) |
| `tools/` | Icon generator, notices generator, packaging script |

### Threading and state

Everything runs on one UI thread. `App` lives in a thread-local and is borrowed through
`with_app`, which returns `None` if the state is already borrowed. **Never** show a menu,
message box, dialog or call `ShellExecute` while holding that borrow, because those run
nested message loops. Read what you need, release the borrow, do the UI work, then borrow
again.

## Building

On Windows (recommended):

```powershell
cargo build --release
cargo test
.\target\release\Berth.exe --self-test report.txt
```

`cargo clippy` and `cargo fmt` are appreciated before sending a pull request.

### Cross-compiling from Linux (optional)

The first version was developed this way. You need MinGW-w64 and a Rust standard library
for `x86_64-pc-windows-gnu`. With rustup:

```sh
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

`.cargo/config.toml` already selects the MinGW linker for that target. Wine can run the
result for rough checks (`winecfg -v win11` first), but Wine doesn't fully implement
several Shell APIs Berth uses (for example `shell:AppsFolder` and some icon APIs), and
popup menus may not draw. Always confirm behavior on Windows 11.

## Regenerating assets

```sh
python tools/make_icon.py      # assets/berth.ico and assets/berth-logo.png (needs Pillow)
python tools/gen_notices.py    # THIRD_PARTY_NOTICES.md (run after changing dependencies)
```

## Releasing

1. Update `version` in `Cargo.toml` and `assets/berth.rc`, and the version in the README's
   install steps.
2. Commit and push to `main` (or merge a pull request).
3. GitHub Actions builds, self-tests and, because that version has no release yet, creates
   the `v<version>` tag and a release with `Berth-<version>-windows-x64.zip` and its
   `.sha256` file.

By contributing you agree that your contributions are licensed under the MIT License.
