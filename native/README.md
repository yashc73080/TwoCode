# TwoCode (native)

A notch at the top of the screen for your media and your Claude Code and Codex sessions. Written in Rust and drawn with Direct2D, with no web engine.

## Build and run

Needs [Rust](https://rustup.rs) and the Visual Studio C++ build tools.

```bash
cargo test
cargo build --release
```

The app is `target/release/twocode.exe`. Right-click the notch or its tray icon for the switches.

## How it works

| File | What it does |
| --- | --- |
| `src/agents.rs` | Turns transcript lines and hook events into one session model |
| `src/watcher.rs` | Follows `~/.claude/projects` and `~/.codex/sessions` |
| `src/hooks.rs` | Optional Allow and Deny: a local hook server and the `settings.json` entries |
| `src/media.rs` | Now playing and controls through the Windows media API |
| `src/layout.rs` | Sizes, motion and when the notch collapses |
| `src/view.rs` | What each state draws and where you can click |
| `src/gfx.rs` | Direct2D and DirectComposition |
| `src/shell.rs` | Tray menu, start with Windows, full-screen detection |

Tracking only reads local files and changes none of your settings. Turning on **Allow or Deny from the notch** adds hooks to `~/.claude/settings.json` or `~/.codex/hooks.json`, keeping a backup next to the file. Codex asks you to trust a new hook once. Turning it off removes them, including any left by the old Electron TwoCode.

When no agent is working, the agents page lists what finished in the last 3 hours. Those runs are saved to `recent.json` on this PC, including what you asked and which files changed, and older ones are dropped. Turn off **Show recent runs** to hide them and stop saving new ones.

Settings and the log are in `%APPDATA%\TwoCode`.
