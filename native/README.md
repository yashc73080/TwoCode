# TwoCode (native)

The Rust app behind TwoCode. It draws with Direct2D and DirectComposition, with no web engine. To install and use it, see the [main README](../README.md).

## Build and test

Needs [Rust](https://rustup.rs) and the Visual Studio C++ build tools.

```powershell
cargo test
cargo build --release
```

The app is `target/release/twocode.exe`. Only one copy runs at a time, so exit the running one before starting a new build.

`cargo test -- --ignored` also runs a test that asks the real LRCLIB service for lyrics.

## How it's organised

| File | What it does |
| --- | --- |
| `src/main.rs` | The window, its state, and what happens on each mouse, keyboard and timer event |
| `src/layout.rs` | Sizes, motion, and when the notch opens and closes. No Windows calls, so it's easy to test |
| `src/view.rs` | What each state draws and where you can click |
| `src/gfx.rs` | Direct2D and DirectComposition drawing |
| `src/agents.rs` | Turns transcript lines and hook events into one session model |
| `src/watcher.rs` | Follows `~/.claude/projects` and `~/.codex/sessions` |
| `src/hooks.rs` | Allow and Deny: a local hook server and the settings entries it adds |
| `src/media.rs` | Now playing and controls, through the Windows media API |
| `src/lyrics.rs` | Synced lyrics from LRCLIB |
| `src/audio.rs` | Bar heights for the waveform, from what Windows is playing |
| `src/bluetooth.rs` | Headphones connecting, and their battery when Windows reports it |
| `src/calendar.rs` | Google sign-in with PKCE, and upcoming events |
| `src/todos.rs` | The todo list and its one-line editor |
| `src/shelf.rs` | Files parked in the notch, with drag in and out |
| `src/menu.rs` | The settings panel |
| `src/shell.rs` | Tray icon, what each setting does, start with Windows, full-screen detection |
| `src/config.rs` | Settings and saved runs in `%APPDATA%\TwoCode` |

## Notes

- **Hooks:** turning on Allow or Deny adds hooks to `~/.claude/settings.json` or `~/.codex/hooks.json` and keeps a backup next to the file. Turning it off removes them, including any left by the old Electron TwoCode.
- **Launching from a packaged app:** a copy started from inside a packaged app, such as the Claude desktop app, can have its `%APPDATA%` writes redirected to that app's private storage. Start it from File Explorer, or with `explorer.exe twocode.exe`, to be sure it uses the real folder.
- **Build number:** setting `TWOCODE_BUILD` when building puts that number in the tray tooltip and the settings panel.
