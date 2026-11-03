# TwoCode (native)

A notch at the top of the screen for your media and your Claude Code and Codex sessions. Written in Rust and drawn with Direct2D, with no web engine.

## Build and run

Needs [Rust](https://rustup.rs) and the Visual Studio C++ build tools.

```bash
cargo test
cargo build --release
```

The app is `target/release/twocode.exe`. Right-click the notch or its tray icon for the settings panel. Move between tabs with the arrows at the ends of the tab row, by scrolling sideways, or by scrolling over the tab row.

## How it works

| File | What it does |
| --- | --- |
| `src/agents.rs` | Turns transcript lines and hook events into one session model |
| `src/watcher.rs` | Follows `~/.claude/projects` and `~/.codex/sessions` |
| `src/hooks.rs` | Optional Allow and Deny: a local hook server and the `settings.json` entries |
| `src/media.rs` | Now playing and controls through the Windows media API |
| `src/bluetooth.rs` | Headphones connecting, and their battery when Windows reports it |
| `src/audio.rs` | Bar heights for the waveform, from what Windows is playing |
| `src/calendar.rs` | Google sign-in and upcoming events |
| `src/todos.rs` | The todo list and its one-line editor |
| `src/shelf.rs` | Files parked in the notch, with drag in and out |
| `src/layout.rs` | Sizes, motion and when the notch collapses |
| `src/view.rs` | What each state draws and where you can click |
| `src/gfx.rs` | Direct2D and DirectComposition |
| `src/menu.rs` | The settings panel |
| `src/shell.rs` | Tray icon, what the settings do, start with Windows, full-screen detection |

Tracking only reads local files and changes none of your settings. Turning on **Allow or Deny from the notch** adds hooks to `~/.claude/settings.json` or `~/.codex/hooks.json`, keeping a backup next to the file. Codex asks you to trust a new hook once. Turning it off removes them, including any left by the old Electron TwoCode.

When no agent is working, the agents page lists what finished in the last 3 hours. Those runs are saved to `recent.json` on this PC, including what you asked and which files changed, and older ones are dropped. Turn off **Show recent runs** to hide them and stop saving new ones.

When headphones or speakers connect or disconnect over Bluetooth, the notch shows them for a few seconds, with the battery level if Windows has one. It only reads what Windows already knows. Turn off **Show headphones connecting** to stop it.

The waveform listens to what your PC is playing only while the media page is open. It keeps nothing but the current bar heights, and never uses the microphone.

Todos are saved to `todos.json`. The shelf saves only file paths to `shelf.json`: it doesn't copy or move anything, and dragging a file out makes a copy.

## Google Calendar

The Calendar tab reads your events and can't change them. It needs your own Google client, created once:

1. In the [Google Cloud Console](https://console.cloud.google.com/), create a project and enable the **Google Calendar API**.
2. Set up the OAuth consent screen, with yourself as a test user.
3. Under **Credentials**, create an **OAuth client ID** of type **Desktop app** and download its JSON.
4. Save that file as `%APPDATA%\TwoCode\google-client.json`, then press **Connect** in the Calendar tab.

Five minutes before a timed event starts, the closed notch shows its name and a countdown. The × puts the pill back for that event.

While the Google project is in testing mode, Google ends the sign-in after 7 days and you have to connect again. Your sign-in is saved encrypted for your Windows account in `google-token.bin`, and **Disconnect Google Calendar** in the menu removes it and asks Google to cancel it.

The AirPods icon is from [Myna UI Icons](https://github.com/praveenjuge/mynaui-icons), MIT licensed; its notice is in `assets/LICENSE-mynaui-icons.txt`.

Settings and the log are in `%APPDATA%\TwoCode`.
