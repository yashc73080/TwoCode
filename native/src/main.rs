#![windows_subsystem = "windows"]
//! TwoCode: a notch at the top of the screen for your media and your coding agents.
mod agents;
mod config;
mod gfx;
mod hooks;
mod layout;
mod lyrics;
mod media;
mod shell;
mod view;
mod watcher;

use agents::{Agent, Hub, SessionView, Status};
use config::Config;
use gfx::{Gfx, Rect};
use layout::{Mode, Motion, Shape, Tab, CANVAS_H, CANVAS_W};
use media::{Command, Media};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use view::{Action, Scene};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_STATE: u32 = WM_APP + 1;
const WM_FOREGROUND: u32 = WM_APP + 3;
const TIMER_OPEN: usize = 1;
const TIMER_CLOSE: usize = 2;
const TIMER_TICK: usize = 3;
const TIMER_FLASH: usize = 4;
const TIMER_FULLSCREEN: usize = 5;
const TIMER_RETRY: usize = 6;
const WM_WHEEL: u32 = 0x020A;
const OPEN_DELAY_MS: u32 = 110;
const CLOSE_DELAY_MS: u32 = 320;
const FLASH_MS: u32 = 4000;

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static STATE_POSTED: AtomicBool = AtomicBool::new(false);

/// Called from background threads. Several changes in a row become one redraw.
fn tell_window() {
    if !STATE_POSTED.swap(true, Ordering::SeqCst) {
        let hwnd = HWND(WINDOW.load(Ordering::SeqCst) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_STATE, WPARAM(0), LPARAM(0));
        }
    }
}

pub struct App {
    hwnd: HWND,
    gfx: Option<Gfx>,
    pub config: Config,
    hub: Arc<Mutex<Hub>>,
    media_state: Arc<Mutex<Media>>,
    media_tx: Sender<Command>,
    lyrics_store: Arc<Mutex<lyrics::Store>>,
    lyrics_tx: Sender<lyrics::Request>,
    lyrics_asked: String,
    lyric_lines: Arc<Vec<lyrics::Line>>,
    pub secret: String,
    media: Media,
    sessions: Vec<SessionView>,
    finished_seen: u64,
    peek_text: String,
    peek_status: Status,
    flashing: bool,
    hovering: bool,
    tracking: bool,
    mode: Mode,
    shape: Shape,
    motion: Option<Motion>,
    last_frame: Instant,
    hits: Vec<(Rect, Action)>,
    hovered: Option<Action>,
    region: (i32, i32),
    hidden_for_fullscreen: bool,
    was_deciding: bool,
    dirty: bool,
    pub tray_message: u32,
}

impl App {
    fn tabs(&self) -> Vec<Tab> {
        layout::tabs(self.config.media, self.config.agents)
    }

    /// The tab to show: a question brings you to the agents, and a switched-off feature gives up its tab.
    fn pick_tab(&mut self) {
        let deciding = self.needs_decision();
        if deciding && !self.was_deciding {
            self.config.tab = Tab::Agents;
        }
        self.was_deciding = deciding;
        let tabs = self.tabs();
        if !tabs.contains(&self.config.tab) {
            if let Some(first) = tabs.first() {
                self.config.tab = *first;
            }
        }
    }

    fn visible_sessions(&self, all: Vec<SessionView>) -> Vec<SessionView> {
        all.into_iter()
            .filter(|s| match s.agent {
                Agent::Claude => self.config.track_claude,
                Agent::Codex => self.config.track_codex,
            })
            .collect()
    }

    fn needs_decision(&self) -> bool {
        self.config.agents && self.sessions.iter().any(|s| s.can_decide)
    }

    /// Reads what the background threads learned and moves the notch if that changes its state.
    fn pull_state(&mut self) {
        STATE_POSTED.store(false, Ordering::SeqCst);
        let now = agents::now_ms();
        let mut finished = None;
        let all = match self.hub.lock() {
            Ok(hub) => {
                if hub.finished_count != self.finished_seen {
                    self.finished_seen = hub.finished_count;
                    finished = hub.last_finished.clone();
                }
                hub.view(now)
            }
            Err(_) => vec![],
        };
        self.sessions = self.visible_sessions(all);
        if let Ok(media) = self.media_state.lock() {
            self.media = media.clone();
        }
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.set_art(self.media.art_rev, self.media.art.as_deref().map(|v| v.as_slice()));
        }
        if let Ok(store) = self.lyrics_store.lock() {
            let key = song_key(&self.media);
            self.lyric_lines = if store.key == key { store.lines.clone() } else { Arc::default() };
        }
        if let Some((agent, project, status)) = finished {
            let tracked = match agent {
                Agent::Claude => self.config.track_claude,
                Agent::Codex => self.config.track_codex,
            };
            if self.config.finish_alerts && self.config.agents && tracked {
                let verb = if status == Status::Error { "hit an error" } else { "finished" };
                self.peek_text = format!("{} {verb} · {project}", agent.name());
                self.peek_status = status;
                self.flashing = true;
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_FLASH, FLASH_MS, None);
                }
            }
        }
        self.settle();
        self.dirty = true;
    }

    /// Starts moving toward whatever the notch should be now.
    pub fn settle(&mut self) {
        self.pick_tab();
        let next = layout::target_mode(self.hovering, self.needs_decision(), self.flashing, !self.tabs().is_empty());
        let to = layout::shape_for(next);
        if next != self.mode || self.motion.as_ref().map_or(self.shape != to, |m| m.to != to) {
            self.mode = next;
            self.motion = Some(Motion { from: self.shape, to, elapsed_ms: 0.0 });
            self.last_frame = Instant::now();
        }
        self.ask_for_lyrics();
        unsafe {
            if next == Mode::Open {
                // Quick enough for a lyric line to change on time; slower when nothing depends on it.
                let singing = self.config.tab == Tab::Media && self.config.lyrics && self.media.playing && !self.lyric_lines.is_empty();
                SetTimer(Some(self.hwnd), TIMER_TICK, if singing { 200 } else { 1000 }, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
        self.dirty = true;
    }

    /// Lyrics are only asked for while you are looking at the media page, once per song.
    fn ask_for_lyrics(&mut self) {
        let looking = self.mode == Mode::Open && self.config.tab == Tab::Media && self.config.media && self.config.lyrics;
        let key = song_key(&self.media);
        if !looking || !self.media.present || self.media.title.is_empty() || key == self.lyrics_asked {
            return;
        }
        self.lyrics_asked = key.clone();
        let _ = self.lyrics_tx.send(lyrics::Request {
            key,
            title: self.media.title.clone(),
            artist: self.media.artist.clone(),
            album: self.media.album.clone(),
            duration_ms: self.media.duration_ms,
        });
    }

    fn next_tab(&mut self, forward: bool) {
        let tabs = self.tabs();
        if tabs.len() < 2 {
            return;
        }
        let at = tabs.iter().position(|t| *t == self.config.tab).unwrap_or(0);
        let next = if forward { (at + 1) % tabs.len() } else { (at + tabs.len() - 1) % tabs.len() };
        self.choose_tab(tabs[next]);
    }

    fn choose_tab(&mut self, tab: Tab) {
        self.config.tab = tab;
        self.config.save();
        self.hovered = None;
        self.settle();
    }

    /// Only the notch itself catches the mouse; everywhere else clicks reach the windows below.
    fn update_region(&mut self) {
        let scale = self.gfx.as_ref().map_or(1.0, |g| g.scale);
        // The pill is too thin to aim for, so its mouse area is a little bigger than what you see, up to the screen edge.
        let (reach_w, reach_h) = if self.shape.h < 12.0 { (28.0, 5.0) } else { (0.0, 0.0) };
        let (w, h) = (((self.shape.w + reach_w) * scale).ceil() as i32 + 2, ((self.shape.y + self.shape.h + reach_h) * scale).ceil() as i32 + 1);
        if (w, h) == self.region {
            return;
        }
        self.region = (w, h);
        let canvas = (CANVAS_W * scale) as i32;
        unsafe {
            let left = (canvas - w) / 2;
            let region = CreateRectRgn(left, 0, left + w, h);
            SetWindowRgn(self.hwnd, Some(region), false);
        }
    }

    fn render(&mut self) {
        if let Some(motion) = self.motion.as_mut() {
            motion.elapsed_ms += self.last_frame.elapsed().as_secs_f32() * 1000.0;
            self.last_frame = Instant::now();
            self.shape = motion.current();
            if motion.finished() {
                self.shape = motion.to;
                self.motion = None;
            }
        }
        // A growing notch needs its room before the frame appears, a shrinking one gives it back after.
        self.update_region();
        let open_h = layout::shape_for(Mode::Open).h;
        let tabs = self.tabs();
        let Some(gfx) = self.gfx.as_mut() else {
            self.dirty = false;
            return;
        };
        let scene = Scene {
            shape: self.shape,
            mode: self.mode,
            open_h,
            tabs: &tabs,
            tab: self.config.tab,
            media: &self.media,
            lyrics_on: self.config.lyrics,
            lyric_lines: &self.lyric_lines,
            sessions: &self.sessions,
            peek: Some(&self.peek_text),
            peek_status: self.peek_status,
            hovered: self.hovered.as_ref(),
        };
        gfx.begin();
        view::draw(gfx, &scene, &mut self.hits);
        if let Err(why) = gfx.end() {
            // The graphics driver restarted or the card changed: build everything again.
            config::log(&format!("drawing failed: {why}"));
            self.recreate_graphics();
            return;
        }
        self.dirty = false;
    }

    /// Creates the surface at this display's scale and puts the window at the top centre of the main screen.
    pub fn recreate_graphics(&mut self) {
        self.gfx = None;
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        let scale = dpi as f32 / 96.0;
        let (w, h) = ((CANVAS_W * scale) as i32, (CANVAS_H * scale) as i32);
        unsafe {
            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), (screen_w - w) / 2, 0, w, h, SWP_NOACTIVATE);
        }
        self.region = (0, 0);
        self.gfx = match Gfx::new(self.hwnd, w as u32, h as u32, scale) {
            Ok(gfx) => Some(gfx),
            Err(why) => {
                // Usually a graphics driver restarting: try again in a moment instead of spinning.
                config::log(&format!("graphics setup failed: {why}"));
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_RETRY, 2000, None);
                }
                None
            }
        };
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.set_art(self.media.art_rev, self.media.art.as_deref().map(|v| v.as_slice()));
        }
        self.dirty = true;
    }

    fn to_canvas(&self, lparam: LPARAM) -> (f32, f32) {
        let scale = self.gfx.as_ref().map_or(1.0, |g| g.scale);
        let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
        (x, y)
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Action> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone())
    }

    fn mouse_move(&mut self, lparam: LPARAM) {
        let hwnd = self.hwnd;
        if !self.tracking {
            let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
            self.tracking = true;
        }
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_CLOSE);
        }
        if !self.hovering {
            unsafe {
                SetTimer(Some(hwnd), TIMER_OPEN, OPEN_DELAY_MS, None);
            }
        }
        let (x, y) = self.to_canvas(lparam);
        let hit = self.hit_at(x, y);
        if hit != self.hovered {
            self.hovered = hit;
            self.dirty = true;
        }
    }

    fn mouse_leave(&mut self) {
        self.tracking = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_OPEN);
            SetTimer(Some(self.hwnd), TIMER_CLOSE, CLOSE_DELAY_MS, None);
        }
        if self.hovered.take().is_some() {
            self.dirty = true;
        }
    }

    fn click(&mut self, lparam: LPARAM) {
        let (x, y) = self.to_canvas(lparam);
        match self.hit_at(x, y) {
            Some(Action::Previous) => drop(self.media_tx.send(Command::Previous)),
            Some(Action::Toggle) => drop(self.media_tx.send(Command::Toggle)),
            Some(Action::Next) => drop(self.media_tx.send(Command::Next)),
            Some(Action::Seek { x: bar, w, duration_ms }) => {
                let at = ((x - bar) / w).clamp(0.0, 1.0) as f64 * duration_ms as f64;
                drop(self.media_tx.send(Command::Seek(at as u64)));
            }
            Some(Action::Allow(agent, id)) => self.answer(agent, &id, true),
            Some(Action::Deny(agent, id)) => self.answer(agent, &id, false),
            Some(Action::Tab(tab)) => self.choose_tab(tab),
            Some(Action::SeekTo(ms)) => drop(self.media_tx.send(Command::Seek(ms))),
            Some(Action::ToggleLyrics) => {
                self.config.lyrics = !self.config.lyrics;
                self.config.save();
                self.settle();
            }
            None => {}
        }
    }

    fn answer(&mut self, agent: Agent, id: &str, allow: bool) {
        if let Ok(mut hub) = self.hub.lock() {
            hub.decide(agent, id, allow);
        }
        self.pull_state();
    }

    fn check_fullscreen(&mut self) {
        let hide = self.config.hide_fullscreen && shell::foreground_is_fullscreen(self.hwnd);
        if hide == self.hidden_for_fullscreen {
            return;
        }
        self.hidden_for_fullscreen = hide;
        unsafe {
            let _ = ShowWindow(self.hwnd, if hide { SW_HIDE } else { SW_SHOWNOACTIVATE });
            if !hide {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        self.dirty = true;
    }

    fn timer(&mut self, id: usize) {
        unsafe {
            if id != TIMER_TICK && id != TIMER_FULLSCREEN {
                let _ = KillTimer(Some(self.hwnd), id);
            }
        }
        match id {
            TIMER_OPEN => {
                self.hovering = true;
                self.settle();
            }
            TIMER_CLOSE => {
                self.hovering = false;
                self.settle();
            }
            TIMER_FLASH => {
                self.flashing = false;
                self.settle();
            }
            TIMER_TICK => self.pull_state(),
            TIMER_FULLSCREEN => self.check_fullscreen(),
            TIMER_RETRY => self.recreate_graphics(),
            _ => {}
        }
    }

    pub fn request_quit(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn song_key(media: &Media) -> String {
    format!("{}\u{1}{}", media.title, media.artist)
}

unsafe fn app<'a>(hwnd: HWND) -> Option<&'a mut App> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App).as_mut()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(app) = app(hwnd) else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    if msg == app.tray_message {
        shell::add_tray(hwnd); // the taskbar restarted
        return LRESULT(0);
    }
    match msg {
        WM_PAINT => {
            // Frames are presented through the composition surface; this only tells Windows it is handled.
            let _ = ValidateRect(Some(hwnd), None);
            app.dirty = true;
        }
        WM_STATE => app.pull_state(),
        WM_FOREGROUND => app.check_fullscreen(),
        WM_TIMER => app.timer(wparam.0),
        WM_MOUSEMOVE => app.mouse_move(lparam),
        WM_MOUSELEAVE => app.mouse_leave(),
        WM_LBUTTONUP => app.click(lparam),
        WM_WHEEL => app.next_tab(((wparam.0 >> 16) as u16 as i16) < 0),
        WM_RBUTTONUP => shell::show_menu(app),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let hand = app.hovered.is_some();
            let cursor = LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).unwrap_or_default();
            SetCursor(Some(cursor));
            return LRESULT(1);
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE => app.recreate_graphics(),
        shell::WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                shell::show_menu(app);
            }
        }
        WM_DESTROY => {
            shell::remove_tray(hwnd);
            PostQuitMessage(0);
        }
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    LRESULT(0)
}

unsafe extern "system" fn foreground_changed(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    let hwnd = HWND(WINDOW.load(Ordering::SeqCst) as *mut _);
    let _ = PostMessageW(Some(hwnd), WM_FOREGROUND, WPARAM(0), LPARAM(0));
}

fn main() {
    unsafe {
        // One notch is enough: a second start just leaves.
        let single = CreateMutexW(None, true, w!("Local\\TwoCodeNotch"));
        if single.is_err() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(PCWSTR::null()).expect("own module");
        let class = w!("TwoCodeNotch");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() });
        let style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
        let hwnd = CreateWindowExW(style, class, w!("TwoCode"), WS_POPUP, 0, 0, 10, 10, None, None, Some(instance.into()), None).expect("the notch window");
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);

        let config = Config::load();
        let hub = Arc::new(Mutex::new(Hub::default()));
        let media_state = Arc::new(Mutex::new(Media::default()));
        let secret = hooks::token(&config::dir());
        hooks::serve(hub.clone(), secret.clone(), tell_window);
        watcher::start(hub.clone(), tell_window);
        let media_tx = media::start(media_state.clone(), tell_window);
        let lyrics_store = Arc::new(Mutex::new(lyrics::Store::default()));
        let lyrics_tx = lyrics::start(lyrics_store.clone(), tell_window);

        let start = layout::shape_for(Mode::Rest);
        let app = Box::into_raw(Box::new(App {
            hwnd,
            gfx: None,
            config,
            hub,
            media_state,
            media_tx,
            lyrics_store,
            lyrics_tx,
            lyrics_asked: String::new(),
            lyric_lines: Arc::default(),
            secret,
            media: Media::default(),
            sessions: vec![],
            finished_seen: 0,
            peek_text: String::new(),
            peek_status: Status::Done,
            flashing: false,
            hovering: false,
            tracking: false,
            mode: Mode::Rest,
            shape: start,
            motion: None,
            last_frame: Instant::now(),
            hits: vec![],
            hovered: None,
            region: (0, 0),
            hidden_for_fullscreen: false,
            was_deciding: false,
            dirty: true,
            tray_message: RegisterWindowMessageW(w!("TaskbarCreated")),
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
        (*app).recreate_graphics();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        shell::add_tray(hwnd);
        SetTimer(Some(hwnd), TIMER_FULLSCREEN, 3000, None);
        let _ = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(foreground_changed), 0, 0, WINEVENT_OUTOFCONTEXT);

        // Between animations this waits in GetMessage, so an idle notch uses no processor time.
        let mut message = MSG::default();
        loop {
            let moving = (*app).motion.is_some();
            if !moving && !(*app).dirty {
                match GetMessageW(&mut message, None, 0, 0).0 {
                    0 | -1 => break,
                    _ => {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            let mut quit = false;
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    quit = true;
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if quit {
                break;
            }
            if (*app).motion.is_some() || (*app).dirty {
                (*app).render();
            }
        }
    }
}
