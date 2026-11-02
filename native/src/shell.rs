//! The Windows parts around the notch: tray icon, the switches menu, start with Windows, full-screen detection.
use crate::agents::Agent;
use crate::hooks;
use crate::menu::Row;
use crate::App;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, SetFocus, KEYEVENTF_KEYUP, VK_MENU};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 2;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("TwoCodeNotch");
const ICON: &[u8] = include_bytes!("../../assets/icon.ico");

fn load_icon() -> HICON {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON);
        let offset = LookupIconIdFromDirectoryEx(ICON.as_ptr(), true, size, size, LR_DEFAULTCOLOR);
        ICON.get(offset.max(0) as usize..)
            .and_then(|bytes| CreateIconFromResourceEx(bytes, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR).ok())
            .unwrap_or_default()
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: load_icon(),
        ..Default::default()
    };
    let tip = format!("TwoCode (build {})", option_env!("TWOCODE_BUILD").unwrap_or("dev"));
    for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16()) {
        *slot = unit;
    }
    data
}

pub fn add_tray(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));
    }
}

pub fn remove_tray(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
    }
}

const ID_MEDIA: usize = 1;
const ID_AGENTS: usize = 2;
const ID_CLAUDE: usize = 3;
const ID_CODEX: usize = 4;
const ID_ALERTS: usize = 5;
const ID_ALLOW: usize = 6;
const ID_FULLSCREEN: usize = 8;
const ID_STARTUP: usize = 9;
const ID_EXIT: usize = 10;
const ID_LYRICS: usize = 11;
const ID_ALLOW_CODEX: usize = 12;
const ID_SOUNDS: usize = 13;
const ID_RECENT: usize = 14;
const ID_CALENDAR: usize = 15;
const ID_TODOS: usize = 16;
const ID_SHELF: usize = 17;
const ID_DISCONNECT: usize = 18;
const ID_DISPLAY_FIRST: usize = 100;

/// What the settings panel shows right now.
pub fn menu_rows(app: &App) -> Vec<Row> {
    let c = &app.config;
    let switch = |id, label: &str, on, enabled, child| Row::Switch { id, label: label.to_string(), on, enabled, child };
    let mut rows = vec![
        Row::Header("Media"),
        switch(ID_MEDIA, "Media player", c.media, true, false),
        switch(ID_LYRICS, "Lyrics", c.lyrics, c.media, true),
        Row::Header("Agents"),
        switch(ID_AGENTS, "Agent tracker", c.agents, true, false),
        switch(ID_CLAUDE, "Follow Claude Code", c.track_claude, c.agents, true),
        switch(ID_CODEX, "Follow Codex", c.track_codex, c.agents, true),
        switch(ID_ALERTS, "Pop up when an agent finishes", c.finish_popup, c.agents, true),
        switch(ID_RECENT, "Show recent runs", c.recent, c.agents, true),
        switch(ID_ALLOW, "Allow or Deny for Claude Code", hooks::installed(&app.secret), c.agents, true),
        switch(ID_ALLOW_CODEX, "Allow or Deny for Codex", hooks::codex_installed(&app.secret), c.agents, true),
        Row::Header("Tabs"),
        switch(ID_CALENDAR, "Calendar", c.calendar, true, false),
    ];
    if crate::calendar::connected() {
        rows.push(Row::Button { id: ID_DISCONNECT, label: "Disconnect Google Calendar".to_string(), enabled: true, danger: false });
    }
    rows.push(switch(ID_TODOS, "Todos", c.todos, true, false));
    rows.push(switch(ID_SHELF, "Shelf", c.shelf, true, false));
    let displays = monitors();
    if displays.len() > 1 {
        rows.push(Row::Header("Show on"));
        let chosen = displays.iter().position(|d| d.name == c.display).or_else(|| displays.iter().position(|d| d.primary));
        for (n, d) in displays.iter().enumerate() {
            let label = format!("Display {} ({} x {}){}", n + 1, d.rect.right - d.rect.left, d.rect.bottom - d.rect.top, if d.primary { ", main" } else { "" });
            rows.push(Row::Radio { id: ID_DISPLAY_FIRST + n, label, on: chosen == Some(n) });
        }
    }
    rows.push(Row::Header("General"));
    rows.push(switch(ID_SOUNDS, "Sounds", c.sounds, true, false));
    rows.push(switch(ID_FULLSCREEN, "Hide in full screen", c.hide_fullscreen, true, false));
    rows.push(switch(ID_STARTUP, "Start with Windows", starts_with_windows(), true, false));
    rows.push(Row::Button { id: ID_EXIT, label: "Exit TwoCode".to_string(), enabled: true, danger: true });
    rows
}

/// Does what a row says. Returns false if the panel should close afterward.
pub fn menu_choose(app: &mut App, id: usize) -> bool {
    if id >= ID_DISPLAY_FIRST {
        if let Some(d) = monitors().get(id - ID_DISPLAY_FIRST) {
            app.config.display = d.name.clone();
            app.config.save();
            app.recreate_graphics();
        }
        return false;
    }
    let (allow_on, allow_codex_on) = (hooks::installed(&app.secret), hooks::codex_installed(&app.secret));
    choose(app, id, allow_on, allow_codex_on);
    id != ID_EXIT && id != ID_DISCONNECT
}

fn choose(app: &mut App, id: usize, allow_on: bool, allow_codex_on: bool) {
    let c = &mut app.config;
    match id {
        ID_MEDIA => c.media = !c.media,
        ID_LYRICS => c.lyrics = !c.lyrics,
        ID_AGENTS => c.agents = !c.agents,
        ID_CLAUDE => c.track_claude = !c.track_claude,
        ID_CODEX => c.track_codex = !c.track_codex,
        ID_ALERTS => c.finish_popup = !c.finish_popup,
        ID_FULLSCREEN => c.hide_fullscreen = !c.hide_fullscreen,
        ID_SOUNDS => c.sounds = !c.sounds,
        ID_RECENT => c.recent = !c.recent,
        ID_CALENDAR => c.calendar = !c.calendar,
        ID_TODOS => c.todos = !c.todos,
        ID_SHELF => c.shelf = !c.shelf,
        ID_DISCONNECT => {
            let _ = app.calendar_tx.send(crate::calendar::Command::Disconnect);
            return;
        }
        ID_ALLOW_CODEX => {
            if let Err(why) = hooks::set_codex_installed(!allow_codex_on, &app.secret) {
                show_error(&why);
            }
            return;
        }
        ID_ALLOW => {
            if let Err(why) = hooks::set_installed(!allow_on, &app.secret) {
                show_error(&why);
            }
            return;
        }
        ID_STARTUP => {
            set_starts_with_windows(!starts_with_windows());
            return;
        }
        ID_EXIT => {
            app.request_quit();
            return;
        }
        _ => return,
    }
    app.config.save();
    app.settle();
    crate::tell_window();
}

fn show_error(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(None, PCWSTR(wide.as_ptr()), w!("TwoCode"), MB_OK | MB_ICONWARNING);
    }
}

fn starts_with_windows() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn set_starts_with_windows(on: bool) {
    unsafe {
        if !on {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let command = format!("\"{}\"", exe.display());
        let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let bytes = (wide.len() * 2) as u32;
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(wide.as_ptr() as *const _), bytes);
    }
}

/// True when the active window covers its whole monitor, like a game, a video or slides.
pub fn foreground_is_fullscreen(own: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == own {
            return false;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(fg, &mut class).max(0) as usize;
        let name = String::from_utf16_lossy(&class[..n]);
        if name == "Progman" || name == "WorkerW" || name == "Shell_TrayWnd" {
            return false; // the desktop is as big as the screen too
        }
        let mut rect = RECT::default();
        if GetWindowRect(fg, &mut rect).is_err() {
            return false;
        }
        let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        // only a full-screen window on the notch's own display hides it
        monitor == MonitorFromWindow(own, MONITOR_DEFAULTTONEAREST) && rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}

// ---- displays ----

pub struct Display {
    pub name: String,
    pub rect: RECT,
    pub primary: bool,
}

unsafe extern "system" fn add_monitor(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
    let list = &mut *(list.0 as *mut Vec<Display>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
        let end = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
        list.push(Display {
            name: String::from_utf16_lossy(&info.szDevice[..end]),
            rect: info.monitorInfo.rcMonitor,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    TRUE
}

pub fn monitors() -> Vec<Display> {
    let mut list: Vec<Display> = vec![];
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(add_monitor), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// The chosen display's area, or the main display's if that one is gone.
pub fn display_rect(chosen: &str) -> RECT {
    let list = monitors();
    let pick = list.iter().find(|d| d.name == chosen).or_else(|| list.iter().find(|d| d.primary)).or(list.first());
    pick.map_or(RECT { left: 0, top: 0, right: unsafe { GetSystemMetrics(SM_CXSCREEN) }, bottom: 0 }, |d| d.rect)
}

/// Hands a web address, file or folder to whatever Windows opens it with.
pub fn open(target: &str) {
    let wide: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Makes the window able to receive typing, and returns what was in front so it can go back there.
pub fn take_focus(hwnd: HWND) -> HWND {
    unsafe {
        let before = GetForegroundWindow();
        if !SetForegroundWindow(hwnd).as_bool() || GetForegroundWindow() != hwnd {
            // The same allowance as jumping to a session: the tap of Alt counts as the user's request.
            keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            let _ = SetForegroundWindow(hwnd);
        }
        let _ = SetFocus(Some(hwnd));
        before
    }
}

pub fn give_focus_back(to: HWND) {
    unsafe {
        if !to.is_invalid() {
            let _ = SetForegroundWindow(to);
        }
    }
}

pub fn clipboard_text(hwnd: HWND) -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let mut text = None;
        if let Ok(handle) = GetClipboardData(CF_UNICODETEXT) {
            let memory = HGLOBAL(handle.0);
            let start = GlobalLock(memory) as *const u16;
            if !start.is_null() {
                let mut length = 0;
                while *start.add(length) != 0 {
                    length += 1;
                }
                text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(start, length)));
                let _ = GlobalUnlock(memory);
            }
        }
        let _ = CloseClipboard();
        text
    }
}

// ---- sounds ----

#[derive(Clone, Copy)]
pub enum Chime {
    Finished,
    Failed,
    NeedsYou,
}

/// One of Windows' own soft sounds. Never waits, and fails silently if the file is missing.
pub fn chime(kind: Chime) {
    let file = match kind {
        Chime::Finished => "Windows Notify System Generic.wav",
        Chime::Failed => "Windows Background.wav",
        Chime::NeedsYou => "Windows Notify Calendar.wav",
    };
    let root = std::env::var_os("windir").unwrap_or_else(|| "C:\\Windows".into());
    let path: Vec<u16> = format!("{}\\Media\\{file}", root.to_string_lossy()).encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = PlaySoundW(PCWSTR(path.as_ptr()), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
    }
}

// ---- jump to a session ----

pub struct WinInfo {
    pub hwnd: HWND,
    pub exe: String,
    pub title: String,
}

/// Which window most likely runs this session. Desktop apps have one window per app;
/// terminals and editors usually name the project in their title.
pub fn pick_window(list: &[WinInfo], agent: Agent, source: &str, project: &str) -> Option<usize> {
    let by_exe = |exe: &str| list.iter().position(|w| w.exe == exe);
    if agent == Agent::Claude && source == "claude-desktop" {
        return by_exe("claude.exe");
    }
    if agent == Agent::Codex && source == "Codex Desktop" {
        if let Some(found) = by_exe("codex.exe") {
            return Some(found);
        }
    }
    const EDITORS: [&str; 3] = ["code.exe", "cursor.exe", "windsurf.exe"];
    const TERMINALS: [&str; 7] = ["windowsterminal.exe", "wezterm-gui.exe", "alacritty.exe", "pwsh.exe", "powershell.exe", "cmd.exe", "conhost.exe"];
    let hosts = |w: &WinInfo| EDITORS.contains(&w.exe.as_str()) || TERMINALS.contains(&w.exe.as_str());
    let needle = project.to_lowercase();
    if !needle.is_empty() {
        if let Some(found) = list.iter().position(|w| hosts(w) && w.title.to_lowercase().contains(&needle)) {
            return Some(found);
        }
    }
    if source.contains("vscode") {
        if let Some(found) = list.iter().position(|w| EDITORS.contains(&w.exe.as_str())) {
            return Some(found);
        }
    }
    list.iter().position(|w| TERMINALS.contains(&w.exe.as_str()))
}

unsafe extern "system" fn add_window(hwnd: HWND, list: LPARAM) -> BOOL {
    let list = &mut *(list.0 as *mut Vec<WinInfo>);
    let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
    let mut title = [0u16; 256];
    let n = GetWindowTextW(hwnd, &mut title).max(0) as usize;
    if IsWindowVisible(hwnd).as_bool() && !owned && n > 0 {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut path = [0u16; 512];
            let mut size = path.len() as u32;
            let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(path.as_mut_ptr()), &mut size).is_ok();
            let _ = CloseHandle(process);
            if ok {
                let full = String::from_utf16_lossy(&path[..size as usize]);
                let exe = full.rsplit('\\').next().unwrap_or("").to_lowercase();
                list.push(WinInfo { hwnd, exe, title: String::from_utf16_lossy(&title[..n]) });
            }
        }
    }
    TRUE
}

/// Brings the session's window to the front. False if no window was found or Windows refused.
pub fn jump_to(agent: Agent, source: &str, project: &str) -> bool {
    let mut list: Vec<WinInfo> = vec![];
    unsafe {
        let _ = EnumWindows(Some(add_window), LPARAM(&mut list as *mut _ as isize));
        let Some(at) = pick_window(&list, agent, source, project) else { return false };
        let hwnd = list[at].hwnd;
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return true;
        }
        // Windows limits who may change the front window; a tap of Alt counts as the user's request.
        keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        let _ = SetForegroundWindow(hwnd);
        GetForegroundWindow() == hwnd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(exe: &str, title: &str) -> WinInfo {
        WinInfo { hwnd: HWND::default(), exe: exe.into(), title: title.into() }
    }

    #[test]
    fn finds_the_right_window() {
        let list = [win("msedge.exe", "webshop - Edge"), win("code.exe", "other - VS Code"), win("windowsterminal.exe", "webshop: claude"), win("claude.exe", "Claude")];
        assert_eq!(pick_window(&list, Agent::Claude, "claude-desktop", "webshop"), Some(3));
        assert_eq!(pick_window(&list, Agent::Claude, "cli", "webshop"), Some(2)); // not Edge, despite its title
        assert_eq!(pick_window(&list, Agent::Codex, "codex_exec", "nothing"), Some(2)); // any terminal
        let editor = [win("code.exe", "a.rs - shop - VS Code")];
        assert_eq!(pick_window(&editor, Agent::Claude, "claude-vscode", "elsewhere"), Some(0));
        assert_eq!(pick_window(&[win("msedge.exe", "x")], Agent::Claude, "cli", "x"), None);
    }
}
