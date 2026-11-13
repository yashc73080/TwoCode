<p align="center">
  <img src="assets/icon-256.png" width="120" alt="TwoCode">
</p>

<h1 align="center">TwoCode</h1>

<p align="center">
  A Windows desktop companion for coding agents, chat apps, music, and focus.
</p>

TwoCode puts a compact “island” and Robo at the top of the desktop. It can follow
Claude Code and OpenAI Codex sessions, monitor an open ChatGPT or Claude window,
answer agent questions, and provide small everyday tools without leaving the
current app.

> **Platform:** Windows 10/11 is the primary target. The app relies on Windows
> PowerShell, Windows UI Automation, and Windows-specific helpers for several
> features.

## What is in the current app?

### Coding-agent monitoring

TwoCode can install and manage its own hooks for:

- **Claude Code** — monitors terminal or VS Code sessions through Claude Code
  hooks and can show permission requests, questions, plans, progress, errors, and
  completion.
- **Codex** — monitors Codex hook events from `~/.codex/hooks.json`.

The island can show the active project, files and tools reported by the agent,
elapsed time, context/usage information when available, and the last 40 finished
turns. Permission requests, multiple-choice questions, plan approvals, and
agent replies can be handled from the island. A response is only sent when the
session and the displayed question still match.

Hook installation edits the existing configuration non-destructively. TwoCode
creates a `.twocode-backup` copy before changing Claude Code or Codex settings.
Disable the connection in **Settings → Connections** to remove only TwoCode’s
hooks.

### Chat-window monitoring

TwoCode can watch one visible, open conversation in the ChatGPT or Claude
desktop/web app. It uses Windows UI Automation rather than browser cookies or
OAuth:

1. Open a logged-in ChatGPT or Claude conversation in its own window.
2. Open **Settings → Connections** and enable **ChatGPT** or **Claude**.
3. Choose the detected conversation window.
4. Keep that window open and visible enough for Windows UI Automation to read it.

The monitor recognizes busy, question, completed, stopped, error, and unavailable
states. It can display a detected question and send a reply after re-checking
the target window, conversation, composer, and question text. It does not claim
that a green state proves work outside the visible conversation has finished.

### Project Mode

Project Mode connects a local project folder to an AI provider and gives you a
project-aware chat in the island. It can:

- index supported text files while skipping generated folders and common secret
  locations;
- select a small relevant set of files for each request;
- answer questions using the current project contents and chat history;
- propose focused changes;
- open the selected file in VS Code through the bundled
  **TwoCode Project Editor** extension;
- apply changes only after validating that the file has not changed;
- create backups and provide an undo path for applied edits.

Project Mode does not scan or send everything in the selected folder. It rejects
known secret files, credentials, private keys, API-key patterns, symlinks, and
unsupported/binary content. Individual editable files are limited to 300 kB.

The Project Editor extension is included at:

```text
vscode-editor/twocode-project-editor.vsix
```

Install it manually when VS Code does not already have it:

```powershell
code --install-extension .\vscode-editor\twocode-project-editor.vsix --force
```

### Robo, settings, and built-in tools

The separate **TwoCode Settings** app includes:

- Home and recent agent history
- Connections for Claude Code, Codex, Claude, and ChatGPT
- AI & Chat settings and ChatGPT sign-in
- Apps and feature visibility
- Robo’s name, appearance, animations, sounds, language, and theme
- Wardrobe/customization
- General settings and notifications
- Release news loaded from the project’s GitHub releases

The island can also provide the following optional tools:

- Spotify playback, seeking, previous/next, and device transfer
- Focus countdown timer
- File tray for up to 24 dropped files
- In-memory history of the last 8 copied text items
- One saved quick note
- Windows master-volume display/control
- Bluetooth battery status and low-battery warnings
- Weather through Open-Meteo, configured by city
- The next event from an HTTPS or `webcal:` ICS calendar link
- Robo Buddy, which can inspect a screen area and use the connected AI to
  explain or help with it

Some tools are controlled by feature packs or the app’s settings. External
services are contacted only when their feature is enabled and configured.

## Install a release

If a Windows release is published, download it from
[Releases](../../releases/latest), extract it, and start the supplied Windows
launcher or executable. TwoCode is portable: it does not require an installer.

Windows may display a SmartScreen warning for an unsigned build. Verify that
the archive came from the project’s official release page before running it.

## Run from source

Install Node.js, then from the repository root run:

```powershell
npm.cmd ci
npm.cmd start
```

On Windows, `START-TWOCODE.cmd` performs the dependency check and starts
Electron. `CONTROLE-WINDOWS.cmd` runs the UI Automation diagnostic.

### AI setup

Use **Settings → Connections** to enable the integrations you need:

- **Claude Code:** TwoCode updates `~/.claude/settings.json` with its local
  hooks and status line.
- **Codex:** TwoCode updates `~/.codex/hooks.json`. Codex may ask you once to
  trust the generated hook commands.
- **ChatGPT for Codex/Project Mode:** install the optional Codex runtime from
  the dependency manifest and sign in from **Settings → AI & Chat**.
- **Claude or ChatGPT window monitoring:** sign in in the original app or
  website, then choose the open window in **Connections**.

Project Mode can also use an OpenAI-compatible configuration. Development
configuration is read from the app’s settings or from
`TWOCODE_OPENAI_API_KEY` / `OPENAI_API_KEY`; do not commit keys to the
repository.

## Keyboard shortcuts

When TwoCode is running:

| Shortcut | Opens |
| --- | --- |
| `Ctrl+Alt+1` | Home |
| `Ctrl+Alt+2` | Spotify |
| `Ctrl+Alt+3` | Claude/agent monitor |
| `Ctrl+Alt+4` | Focus |
| `Ctrl+Alt+5` | Clipboard |
| `Ctrl+Alt+6` | Note |

The Robo tray icon can open the island, open Settings, enable or disable
starting with Windows, restart the app, or quit TwoCode.

## Privacy and safety

TwoCode does not require a TwoCode account. The coding-agent hook server listens
on `127.0.0.1` and authenticates hook requests with a locally generated token.
Chat-window monitoring reads accessibility data from the selected local window;
it does not read browser cookies or passwords.

Project Mode is different: selected project context is sent to the AI provider
configured by the user so that the provider can answer or produce an edit plan.
The app uses local file checks, size limits, secret detection, path validation,
hash checks, and backups before applying edits. Weather, calendar, Spotify, and
ChatGPT/Claude integrations communicate with their respective services when
enabled.

## Troubleshooting

### ChatGPT or Claude is not detected

1. Open the conversation in a separate, logged-in window.
2. Make sure the conversation and composer are visible and not blocked by a
   lock screen or another tab.
3. In **Settings → Connections**, refresh the window list and reconnect it.
4. Run the diagnostic:

   ```powershell
   npm.cmd run check:windows
   ```

The diagnostic reports whether a provider, input field, assistant response, and
completion controls were recognized. It does not send a message or print
conversation contents.

### Claude Code or Codex does not appear

Check that the relevant CLI is installed and that its configuration file is
valid. Disable and re-enable the integration from **Connections**. Restart the
CLI once after hook installation. TwoCode’s local hook endpoint uses port
`47823`; a local process already using that port can prevent monitoring.

### Project Mode cannot edit a file

The file may be binary, larger than 300 kB, changed since it was read, inside a
blocked directory, linked through a symlink, or identified as containing
credentials or a private key. Save the file as UTF-8, reopen the project, and
send the request again.

## Development

The repository is a CommonJS Electron application. The main process is
`main.js`; the island UI is loaded from `mascot.html`. Feature code is split
between main-process modules, preload bridges, and UI modules such as
`ai-monitor-*`, `project-*`, `extras-*`, `spotify-ui.js`, and `focus-ui.js`.

Useful commands:

```powershell
# Install exactly from the lockfile
npm.cmd ci

# Start Electron
npm.cmd start

# Run the focused Node test suite
npm.cmd test

# Check Windows UI Automation integration
npm.cmd run check:windows
```

Tests cover monitor state transitions and guarded replies, Claude Code/Codex
hook behavior, Windows checks, and extras. The real ChatGPT/Claude UI and
Windows notification behavior still need to be tested on a Windows machine.

## License and project links

The package currently declares the `ISC` license in `package.json`.

- Website: [twocode.nl](https://twocode.nl)
- Releases: [github.com/yashc73080/TwoCode/releases](../../releases)
