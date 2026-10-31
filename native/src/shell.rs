//! The Windows parts around the notch: tray icon, the switches menu, start with Windows, full-screen detection.
use crate::hooks;
use crate::App;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
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

pub fn show_menu(app: &mut App) {
    let allow_on = hooks::installed(&app.secret);
    let c = app.config.clone();
    let items: [(usize, &str, bool, bool); 9] = [
        (ID_MEDIA, "Media player", c.media, true),
        (ID_LYRICS, "    Lyrics (from LRCLIB)", c.lyrics, c.media),
        (ID_AGENTS, "Agent tracker", c.agents, true),
        (ID_CLAUDE, "    Follow Claude Code", c.track_claude, c.agents),
        (ID_CODEX, "    Follow Codex", c.track_codex, c.agents),
        (ID_ALERTS, "    Pop up when an agent finishes", c.finish_popup, c.agents),
        (ID_ALLOW, "    Allow or Deny from the notch (Claude Code)", allow_on, c.agents),
        (ID_FULLSCREEN, "Hide in full screen", c.hide_fullscreen, true),
        (ID_STARTUP, "Start with Windows", starts_with_windows(), true),
    ];
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, (id, label, checked, enabled)) in items.iter().enumerate() {
            let wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let mut flags = MF_STRING;
            if *checked {
                flags |= MF_CHECKED;
            }
            if !*enabled {
                flags |= MF_GRAYED;
            }
            let _ = AppendMenuW(menu, flags, *id, PCWSTR(wide.as_ptr()));
            if i == 6 || i == 8 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
        }
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit TwoCode"));
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let hwnd = HWND(crate::WINDOW.load(std::sync::atomic::Ordering::SeqCst) as *mut _);
        let _ = SetForegroundWindow(hwnd); // otherwise the menu does not close when you click elsewhere
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, at.x, at.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        choose(app, chosen.0 as usize, allow_on);
    }
}

fn choose(app: &mut App, id: usize, allow_on: bool) {
    let c = &mut app.config;
    match id {
        ID_MEDIA => c.media = !c.media,
        ID_LYRICS => c.lyrics = !c.lyrics,
        ID_AGENTS => c.agents = !c.agents,
        ID_CLAUDE => c.track_claude = !c.track_claude,
        ID_CODEX => c.track_codex = !c.track_codex,
        ID_ALERTS => c.finish_popup = !c.finish_popup,
        ID_FULLSCREEN => c.hide_fullscreen = !c.hide_fullscreen,
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
        // the notch lives on the main screen, so a full-screen window elsewhere does not hide it
        m.left == 0 && m.top == 0 && rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}
