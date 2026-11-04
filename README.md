<p align="center"><img src="assets/icon-256.png" width="120" alt="TwoCode icon"></p>

<h1 align="center">TwoCode</h1>
<p align="center"><b>A notch for Windows that shows your music and your Claude Code and Codex agents.</b><br>
It rests as a thin pill at the top of the screen and opens when you point at it.</p>

<p align="center">Windows 10 and 11 · Rust, with no web engine · about 50 MB of memory</p>

---

## What it does

| Tab | What you get |
| --- | --- |
| **Media** | Whatever is playing in any app, with controls, a seek bar, synced lyrics from [LRCLIB](https://lrclib.net) and a waveform that follows the sound |
| **Agents** | Every Claude Code and Codex session on this PC, from the terminal, an editor or the desktop apps: what it's doing now, its task list, and whether it's stuck or waiting for you |
| **Calendar** | Your Google Calendar events for the next 7 days, with a Join button for video calls |
| **Todos** | A quick list you can add to, edit, tick off and delete right in the notch |
| **Shelf** | Drag files onto the pill to park them, then drag them back out when you need them |

The closed pill also tells you things without opening:

- **Its colour** follows your agents: blue while one is working, green when it finishes, red if it hits an error. It goes back to black once you've looked.
- **Lyrics:** close the notch from the lyrics view and it keeps showing the current line.
- **Meetings:** five minutes before an event, it shows the name and a countdown. The × dismisses it.
- **Headphones:** when Bluetooth headphones connect or disconnect, it pops open with their name, plus the battery level when Windows reports one.

With **Allow or Deny** turned on, you can approve an agent's command or edit from the notch, with a preview of what it will run or change.

## Install

There's no ready-made download yet, so you build TwoCode from source. It takes about 10 minutes the first time, mostly installing Rust.

1. **Install Rust** from [rustup.rs](https://rustup.rs). When the installer offers the Visual Studio C++ build tools, accept them.
2. **Build TwoCode.** Open PowerShell and run:

   ```powershell
   git clone -b native-rust-notch https://github.com/yashc73080/TwoCode.git
   cd TwoCode\native
   cargo build --release
   ```

3. **Put it somewhere permanent.** The build folder is overwritten by every rebuild, so copy the app out of it:

   ```powershell
   New-Item -ItemType Directory -Force "$env:LOCALAPPDATA\Programs\TwoCode" | Out-Null
   Copy-Item target\release\twocode.exe "$env:LOCALAPPDATA\Programs\TwoCode\"
   ```

4. **Start it** by double-clicking `twocode.exe` in `%LOCALAPPDATA%\Programs\TwoCode`. The pill appears at the top of your main screen.
5. **Keep it running:** right-click the pill and turn on **Start with Windows**.

**If Windows blocks it:** your build isn't signed, so Windows may warn about it.
- **SmartScreen** ("Windows protected your PC"): click **More info**, then **Run anyway**.
- **Smart App Control:** this has no per-app exception. Running `cargo build --release` again produces a new copy, which Windows sometimes allows.

### Update

Exit TwoCode from its settings panel, then from the `TwoCode\native` folder:

```powershell
git pull
cargo build --release
Copy-Item target\release\twocode.exe "$env:LOCALAPPDATA\Programs\TwoCode\" -Force
```

Start it again. Your settings, todos and shelf are kept.

### Uninstall

1. In the settings panel, turn off **Allow or Deny** for Claude Code and Codex if you turned them on. This removes TwoCode's hooks from their settings.
2. Turn off **Start with Windows**, then choose **Exit TwoCode**.
3. Delete `%LOCALAPPDATA%\Programs\TwoCode` and `%APPDATA%\TwoCode`.

## Using it

- **Open:** point at the pill. It closes when the mouse leaves.
- **Switch tabs:** click the dots at the bottom, use the faint arrows on each side, or swipe sideways on a touchpad.
- **Settings:** right-click the notch or the tray icon. Each tab and feature has its own switch there.

### Claude Code and Codex

Tracking works as soon as TwoCode runs: it reads the session files Claude Code and Codex already write to `~/.claude` and `~/.codex`. Agents running in the cloud or on another machine don't appear.

To approve commands from the notch, turn on **Allow or Deny for Claude Code** or **for Codex** in the settings panel. That adds a hook to `~/.claude/settings.json` or `~/.codex/hooks.json`, with a backup next to the file. Codex asks you to trust the new hook once. A request you don't answer in the notch goes back to the agent's own window after 5 minutes for Claude Code, or 1 minute for Codex.

### Google Calendar

The Calendar tab can read your events but never change them. It needs your own Google client, which you create once:

1. In the [Google Cloud Console](https://console.cloud.google.com/), create a project and enable the **Google Calendar API**.
2. Set up the OAuth consent screen and add yourself as a test user.
3. Under **Credentials**, create an **OAuth client ID** of type **Desktop app** and download its JSON file.
4. Save it as `%APPDATA%\TwoCode\google-client.json` (the tab's **Open folder** button takes you there), then press **Connect**.

While your Google project is in testing mode, Google ends the sign-in after 7 days and you'll need to connect again.

## Privacy

TwoCode runs on your PC with no account. It only connects to:

- **LRCLIB:** the title, artist, album and length of the current song, when lyrics are on.
- **Google:** to sign in and read your calendar, only if you connect it.

Everything else stays on your PC. The waveform reads what your speakers are playing, never the microphone, and keeps only the current bar heights.

Your data lives in `%APPDATA%\TwoCode`:

| File | What's in it |
| --- | --- |
| `config.json` | Your settings |
| `recent.json` | Agent runs from the last 3 hours: what you asked and which files changed |
| `todos.json` | Your todos |
| `shelf.json` | Paths of files on the shelf. The files themselves aren't copied |
| `google-token.bin` | Your Google sign-in, encrypted for your Windows account |
| `twocode.log` | Problems worth knowing about, capped at 64 KB |

## For developers

The app is in [`native/`](native/), and [`native/README.md`](native/README.md) explains how it's built and tested. The JavaScript files in the repository root belong to the original Electron version of TwoCode, which this rebuild replaces.

The AirPods icon is from [Myna UI Icons](https://github.com/praveenjuge/mynaui-icons) (MIT). Its notice is in [`native/assets/LICENSE-mynaui-icons.txt`](native/assets/LICENSE-mynaui-icons.txt).
