#![windows_subsystem = "windows"]
//! TwoCode: a notch at the top of the screen for your media and your coding agents.
mod agents;
mod audio;
mod calendar;
mod config;
mod gfx;
mod hooks;
mod layout;
mod lyrics;
mod media;
mod menu;
mod shelf;
mod shell;
mod todos;
mod view;
mod watcher;

use agents::{Agent, Hub, Run, SessionView, Status};
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
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_BACK, VK_CONTROL, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT};
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
const TIMER_WAVE: usize = 7;
const TIMER_REMINDER: usize = 8;
const REMINDER_CHECK_MS: u32 = 15_000;
const WAVE_FRAME_MS: u32 = 33;
const WM_WHEEL: u32 = 0x020A;
const DRAG_START_PX: f32 = 5.0;
const AFTER_DROP_MS: u32 = 1500;
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
    selected: Option<(Agent, String)>,
    auto_selected: bool,
    viewed_agents: bool,
    runs: Vec<Run>,
    selected_run: Option<u64>,
    runs_scroll: usize,
    page_scroll: usize,
    todos: Vec<todos::Todo>,
    edit: Option<todos::Edit>,
    focus_before: HWND,
    high_surrogate: Option<u16>,
    shelf: Vec<shelf::Item>,
    shelf_dropped: Arc<Mutex<Vec<String>>>,
    shelf_receiving: bool,
    pressed: Option<(String, f32, f32)>,
    calendar_state: Arc<Mutex<calendar::State>>,
    calendar: calendar::State,
    calendar_tx: Sender<calendar::Command>,
    wave_state: Arc<Mutex<audio::Levels>>,
    wave: audio::Levels,
    audio_tx: Sender<bool>,
    listening: bool,
    lyric_strip: bool,
    opened_by_hover: bool,
    menu: Option<HWND>,
    menu_holds_open: bool,
    reminder: Option<calendar::Event>,
    dismissed: Vec<(String, u64)>,
    tab_reveal: f32,
    tab_position: f32,
    tabs_clock: Instant,
    dirty: bool,
    pub tray_message: u32,
}

impl App {
    /// Opening the notch counts as seeing the pill's news. Closing it after looking
    /// at the agents page lets finished work and errors leave the list.
    fn note_viewing(&mut self, next: Mode) {
        let opening = next == Mode::Open && self.mode != Mode::Open;
        let closing = next != Mode::Open && self.mode == Mode::Open;
        let viewed = closing && self.viewed_agents;
        if let Ok(mut hub) = self.hub.lock() {
            if opening || viewed {
                hub.acknowledge();
            }
            if viewed {
                hub.mark_viewed();
            }
        }
        if closing {
            self.viewed_agents = false;
        }
        if next == Mode::Open && self.config.tab == Tab::Agents {
            self.viewed_agents = true;
        }
        if opening || viewed {
            let all = self.hub.lock().map(|hub| hub.view(agents::now_ms())).unwrap_or_default();
            self.sessions = self.visible_sessions(all);
        }
    }

    fn play(&self, chime: shell::Chime) {
        if self.config.sounds {
            shell::chime(chime);
        }
    }

    fn selected_view(&self) -> Option<&SessionView> {
        let (agent, id) = self.selected.as_ref()?;
        self.sessions.iter().find(|s| s.agent == *agent && s.id == *id)
    }

    fn open_width(&self) -> f32 {
        let lyrics = self.config.lyrics && !self.lyric_lines.is_empty() && self.media.present;
        let now = agents::now_ms();
        let nothing_listed = match self.config.tab {
            Tab::Calendar => self.calendar.phase != calendar::Phase::Ready || !self.calendar.events.iter().any(|e| e.end_ms > now),
            _ => self.sessions.is_empty() && visible_runs(&self.runs, &self.config, now).is_empty(),
        };
        layout::open_width(self.config.tab, lyrics, nothing_listed)
    }

    fn tabs(&self) -> Vec<Tab> {
        layout::tabs(self.config.media, self.config.agents, self.config.calendar, self.config.todos, self.config.shelf)
    }

    /// The tab to show: a question brings you to the agents, and a switched-off feature gives up its tab.
    fn pick_tab(&mut self) {
        let deciding = self.needs_decision();
        if deciding && !self.was_deciding {
            self.config.tab = Tab::Agents;
            self.play(shell::Chime::NeedsYou);
        }
        // A question opens on that session, and moves on when it is answered.
        let waiting = self.sessions.iter().find(|s| s.can_decide).map(|s| (s.agent, s.id.clone()));
        let selected_decides = self.selected_view().is_some_and(|s| s.can_decide);
        if deciding && !selected_decides && (!self.was_deciding || self.auto_selected) {
            self.selected = waiting;
            self.auto_selected = true;
        } else if !deciding && self.was_deciding && self.auto_selected {
            self.selected = None;
            self.auto_selected = false;
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
            Ok(mut hub) => {
                if hub.finished_count != self.finished_seen {
                    self.finished_seen = hub.finished_count;
                    finished = hub.last_finished.clone();
                }
                if hub.recent_changed {
                    hub.recent_changed = false;
                    self.runs = hub.recent.clone();
                    if self.config.recent {
                        config::save_runs(&self.runs);
                    }
                }
                hub.view(now)
            }
            Err(_) => vec![],
        };
        self.sessions = self.visible_sessions(all);
        if let Ok(media) = self.media_state.lock() {
            self.media = media.clone();
        }
        if let Ok(state) = self.calendar_state.lock() {
            self.calendar = state.clone();
        }
        if self.mode == Mode::Open && self.config.tab == Tab::Shelf {
            let before = self.shelf.len();
            self.shelf.retain(|item| std::path::Path::new(&item.path).exists());
            if self.shelf.len() != before {
                shelf::save(&self.shelf);
            }
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
            if self.config.agents && tracked {
                self.play(match status {
                    Status::Error => shell::Chime::Failed,
                    Status::Waiting => shell::Chime::NeedsYou,
                    _ => shell::Chime::Finished,
                });
            }
            if self.config.finish_popup && self.config.agents && tracked {
                let verb = match status {
                    Status::Error => "hit an error",
                    Status::Waiting => "needs you",
                    _ => "finished",
                };
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
        shelf::ACCEPTS.store(self.config.shelf, Ordering::SeqCst);
        if self.edit.is_some() && self.config.tab != Tab::Todos {
            self.commit_edit();
            self.give_focus_back();
        }
        // Typing or dragging a file holds the notch open, like the mouse does.
        let held = self.hovering || self.edit.is_some() || self.shelf_receiving || self.menu_holds_open;
        let has_tabs = !self.tabs().is_empty();
        self.reminder = self.current_reminder();
        let strip = self.reminder.is_some() || self.lyric_strip_ready();
        let mut next = layout::target_mode(held, self.needs_decision(), self.flashing, has_tabs, strip);
        // Only closing it yourself from the lyrics view leaves the line showing. A question that opened it decides nothing.
        if next != Mode::Open && self.mode == Mode::Open {
            if self.opened_by_hover {
                self.lyric_strip = self.lyrics_view_showing();
            }
            self.opened_by_hover = false;
            let strip = self.reminder.is_some() || self.lyric_strip_ready();
            next = layout::target_mode(held, self.needs_decision(), self.flashing, has_tabs, strip);
        }
        if next == Mode::Open && self.hovering {
            self.opened_by_hover = true;
        }
        self.listen_to_audio(next == Mode::Open && self.config.tab == Tab::Media && self.config.media);
        if next == Mode::Open && self.mode != Mode::Open && self.config.tab == Tab::Calendar {
            let _ = self.calendar_tx.send(calendar::Command::Refresh);
        }
        self.note_viewing(next);
        if matches!(next, Mode::Rest | Mode::Lyric) {
            self.selected = None;
            self.selected_run = None;
            self.runs_scroll = 0;
            self.page_scroll = 0;
            self.pressed = None;
        }
        let to = match next {
            Mode::Open => layout::open_shape(self.open_width()),
            Mode::Lyric => layout::lyric_shape(self.lyric_width()),
            _ => layout::shape_for(next),
        };
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
            } else if next == Mode::Lyric {
                // Wakes when the line changes, and otherwise rarely.
                let next_line = if self.reminder.is_some() { None } else { lyrics::ms_to_next(&self.lyric_lines, self.media.position_now()) };
                let wait = next_line.unwrap_or(1000).clamp(40, 1000);
                SetTimer(Some(self.hwnd), TIMER_TICK, wait as u32, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
        self.dirty = true;
    }

    fn lyrics_view_showing(&self) -> bool {
        self.config.tab == Tab::Media && self.config.media && self.config.lyrics && self.media.present && !self.lyric_lines.is_empty()
    }

    /// Paused, or a song with no timed lyrics, goes back to the plain pill until there is a line to follow.
    fn lyric_strip_ready(&self) -> bool {
        self.lyric_strip && self.config.media && self.config.lyrics && self.media.present && self.media.playing && !self.lyric_lines.is_empty()
    }

    fn current_reminder(&self) -> Option<calendar::Event> {
        if !self.config.calendar || self.calendar.phase != calendar::Phase::Ready {
            return None;
        }
        calendar::reminder(&self.calendar.events, agents::now_ms(), &self.dismissed).cloned()
    }

    fn lyric_width(&mut self) -> f32 {
        if let Some(event) = &self.reminder {
            let text = calendar::reminder_text(event, agents::now_ms());
            return self.gfx.as_mut().map_or(0.0, |g| g.text_width(&text, layout::LYRIC_TEXT, true)) + layout::STRIP_CLOSE_W;
        }
        let position = self.media.position_now();
        let text = lyrics::current_line(&self.lyric_lines, position).map_or("♪".to_string(), |i| self.lyric_lines[i].text.clone());
        self.gfx.as_mut().map_or(0.0, |g| g.text_width(&text, layout::LYRIC_TEXT, true))
    }

    /// The waveform listens only while you can see it.
    fn listen_to_audio(&mut self, wanted: bool) {
        if wanted == self.listening {
            return;
        }
        self.listening = wanted;
        let _ = self.audio_tx.send(wanted);
        unsafe {
            if wanted {
                SetTimer(Some(self.hwnd), TIMER_WAVE, WAVE_FRAME_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_WAVE);
            }
        }
        if !wanted {
            self.wave = [0.0; audio::BARS];
        }
    }

    /// Lyrics are asked for on the media page, or for the next song while its line is on the notch. Once per song.
    fn ask_for_lyrics(&mut self) {
        let on_page = self.mode == Mode::Open && self.config.tab == Tab::Media;
        let looking = (on_page || self.lyric_strip) && self.config.media && self.config.lyrics;
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

    fn jump(&mut self, agent: Agent, id: &str) {
        let Some(session) = self.sessions.iter().find(|s| s.agent == agent && s.id == id) else { return };
        let found = shell::jump_to(agent, &session.source, &session.project);
        self.after_jump(agent, found);
    }

    fn jump_to_run(&mut self, at_ms: u64) {
        let Some(run) = self.runs.iter().find(|r| r.at_ms == at_ms) else { return };
        let (agent, found) = (run.agent, shell::jump_to(run.agent, &run.source, &run.project));
        self.after_jump(agent, found);
    }

    fn after_jump(&mut self, agent: Agent, found: bool) {
        if !found {
            self.peek_text = format!("Could not find the {} window", agent.name());
            self.peek_status = Status::Waiting;
            self.flashing = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_FLASH, FLASH_MS, None);
            }
            self.settle();
        }
    }

    fn begin_edit(&mut self, target: Option<u64>) {
        let text = target.and_then(|id| self.todos.iter().find(|t| t.id == id)).map_or(String::new(), |t| t.text.clone());
        self.edit = Some(todos::Edit::new(target, &text));
        self.high_surrogate = None;
        let before = shell::take_focus(self.hwnd);
        if before != self.hwnd {
            self.focus_before = before;
        }
        self.dirty = true;
    }

    /// Saves what was typed. Does nothing if no typing is going on.
    fn commit_edit(&mut self) {
        let Some(edit) = self.edit.take() else { return };
        match edit.target {
            Some(id) => todos::rewrite(&mut self.todos, id, &edit.text()),
            None => todos::add(&mut self.todos, &edit.text()),
        }
        todos::save(&self.todos);
        self.dirty = true;
    }

    fn give_focus_back(&mut self) {
        shell::give_focus_back(self.focus_before);
        self.focus_before = HWND::default();
    }

    fn key_down(&mut self, key: u32) {
        let Some(edit) = self.edit.as_mut() else { return };
        const VK_V: u32 = 0x56;
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match key {
            k if k == VK_LEFT.0 as u32 => edit.left(),
            k if k == VK_RIGHT.0 as u32 => edit.right(),
            k if k == VK_HOME.0 as u32 => edit.home(),
            k if k == VK_END.0 as u32 => edit.end(),
            k if k == VK_BACK.0 as u32 => edit.backspace(),
            k if k == VK_DELETE.0 as u32 => edit.delete(),
            k if k == VK_V && ctrl => {
                if let Some(text) = shell::clipboard_text(self.hwnd) {
                    edit.insert(&text);
                }
            }
            k if k == VK_ESCAPE.0 as u32 => {
                self.edit = None;
                self.give_focus_back();
                self.settle();
            }
            k if k == VK_RETURN.0 as u32 => {
                let adding = edit.target.is_none() && !edit.chars.is_empty();
                self.commit_edit();
                if adding {
                    // Enter again for the next one, or an empty Enter to finish.
                    self.edit = Some(todos::Edit::default());
                } else {
                    self.give_focus_back();
                    self.settle();
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn typed(&mut self, unit: u16) {
        let Some(edit) = self.edit.as_mut() else { return };
        let high = self.high_surrogate.take();
        if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = Some(unit);
            return;
        }
        let units = match high {
            Some(first) => vec![first, unit],
            None => vec![unit],
        };
        edit.insert(&String::from_utf16_lossy(&units));
        self.dirty = true;
    }

    fn take_dropped_files(&mut self) {
        let paths: Vec<String> = self.shelf_dropped.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default();
        if shelf::add(&mut self.shelf, &paths) {
            shelf::save(&self.shelf);
        }
        self.shelf_receiving = false;
        // Stay open a moment so you can see them arrive.
        self.hovering = true;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_CLOSE, AFTER_DROP_MS, None);
        }
        self.settle();
    }

    fn drag_arrived(&mut self, arriving: bool) {
        self.shelf_receiving = arriving;
        if arriving && self.config.shelf {
            self.config.tab = Tab::Shelf;
        }
        self.settle();
    }

    /// The wheel scrolls a page with more than fits, and otherwise moves between tabs.
    fn wheel(&mut self, down: bool) {
        let over_tabs = matches!(self.hovered, Some(Action::Tab(_)));
        let last = visible_runs(&self.runs, &self.config, agents::now_ms()).len().saturating_sub(view::RUN_ROWS);
        let on_runs = self.config.tab == Tab::Agents && self.selected.is_none() && self.selected_run.is_none() && !agents::any_live(&self.sessions);
        if !over_tabs && on_runs && last > 0 {
            self.runs_scroll = if down { (self.runs_scroll + 1).min(last) } else { self.runs_scroll.saturating_sub(1) };
            self.hovered = None;
            self.dirty = true;
            return;
        }
        let now = agents::now_ms();
        let last = match self.config.tab {
            Tab::Todos => self.todos.len().saturating_sub(view::TODO_ROWS),
            Tab::Shelf => self.shelf.len().saturating_sub(view::SHELF_COLUMNS),
            Tab::Calendar if self.calendar.phase == calendar::Phase::Ready => self.calendar.events.iter().filter(|e| e.end_ms > now).count().saturating_sub(view::EVENT_ROWS),
            _ => 0,
        };
        if !over_tabs && last > 0 {
            self.page_scroll = if down { (self.page_scroll + 1).min(last) } else { self.page_scroll.saturating_sub(1) };
            self.hovered = None;
            self.dirty = true;
        } else {
            self.next_tab(down);
        }
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
        self.page_scroll = 0;
        // Still pointing at the tabs: they stay as icons until the mouse leaves them.
        if !matches!(self.hovered, Some(Action::Tab(_))) {
            self.hovered = None;
        }
        if tab == Tab::Calendar {
            let _ = self.calendar_tx.send(calendar::Command::Refresh);
        }
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
        let picker_moving = self.move_tab_picker(&tabs);
        let reminder_text = self.reminder.as_ref().map(|e| calendar::reminder_text(e, agents::now_ms()));
        let now_ms = agents::now_ms();
        let runs = visible_runs(&self.runs, &self.config, now_ms);
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
            selected: self.selected.as_ref().and_then(|(agent, id)| self.sessions.iter().find(|s| s.agent == *agent && s.id == *id)),
            selected_run: self.selected_run.and_then(|at| runs.iter().find(|r| r.at_ms == at).copied()),
            runs: &runs,
            runs_scroll: self.runs_scroll,
            following: (self.config.track_claude, self.config.track_codex),
            todos: &self.todos,
            edit: self.edit.as_ref(),
            shelf: &self.shelf,
            shelf_receiving: self.shelf_receiving,
            calendar: &self.calendar,
            page_scroll: self.page_scroll,
            wave: &self.wave,
            reminder: reminder_text.as_deref(),
            tab_reveal: self.tab_reveal,
            tab_position: self.tab_position,
            now_ms,
            pill: if self.config.agents { agents::pill_state(&self.sessions) } else { agents::Pill::Quiet },
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
        self.dirty = picker_moving;
    }

    /// One step of the tab picker's animation. True if it needs another frame.
    fn move_tab_picker(&mut self, tabs: &[Tab]) -> bool {
        let seconds = self.tabs_clock.elapsed().as_secs_f32().min(0.05);
        self.tabs_clock = Instant::now();
        let position = tabs.iter().position(|t| *t == self.config.tab).unwrap_or(0) as f32;
        if self.mode != Mode::Open {
            self.tab_reveal = 0.0;
            self.tab_position = position;
            return false;
        }
        let reveal = if matches!(self.hovered, Some(Action::Tab(_))) { 1.0 } else { 0.0 };
        self.tab_reveal = layout::ease_toward(self.tab_reveal, reveal, seconds);
        self.tab_position = layout::ease_toward(self.tab_position, position, seconds);
        self.tab_reveal != reveal || self.tab_position != position
    }

    /// Creates the surface at this display's scale and puts the window at the top centre of the main screen.
    pub fn recreate_graphics(&mut self) {
        self.gfx = None;
        let area = shell::display_rect(&self.config.display);
        // Move first: the display decides the scale.
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), area.left, area.top, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE);
        }
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        let scale = dpi as f32 / 96.0;
        let (w, h) = ((CANVAS_W * scale) as i32, (CANVAS_H * scale) as i32);
        unsafe {
            let screen_w = area.right - area.left;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), area.left + (screen_w - w) / 2, area.top, w, h, SWP_NOACTIVATE);
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

    fn mouse_down(&mut self, lparam: LPARAM) {
        let (x, y) = self.to_canvas(lparam);
        self.pressed = match self.hit_at(x, y) {
            Some(Action::ShelfOpen(path)) => Some((path, x, y)),
            _ => None,
        };
    }

    fn mouse_move(&mut self, wparam: WPARAM, lparam: LPARAM) {
        let hwnd = self.hwnd;
        if let Some((path, from_x, from_y)) = self.pressed.clone() {
            let (x, y) = self.to_canvas(lparam);
            let button_down = wparam.0 & 1 != 0;
            if !button_down {
                self.pressed = None;
            } else if (x - from_x).abs().max((y - from_y).abs()) > DRAG_START_PX {
                self.pressed = None;
                shelf::drag_out(&path);
                self.hovered = None;
                return;
            }
        }
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
        let (x, y) = self.to_canvas(lparam);
        let hit = self.hit_at(x, y);
        // Aiming for the reminder's × shouldn't open the notch out from under it.
        unsafe {
            if matches!(hit, Some(Action::DismissReminder)) {
                let _ = KillTimer(Some(hwnd), TIMER_OPEN);
            } else if !self.hovering {
                SetTimer(Some(hwnd), TIMER_OPEN, OPEN_DELAY_MS, None);
            }
        }
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
        let hit = self.hit_at(x, y);
        let was_pressed = self.pressed.take().is_some();
        if self.edit.is_some() && !matches!(hit, Some(Action::TodoField)) {
            self.commit_edit();
            if !matches!(hit, Some(Action::TodoNew | Action::TodoEdit(_))) {
                self.give_focus_back();
            }
        }
        match hit {
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
            Some(Action::Open(agent, id)) => {
                self.selected = Some((agent, id));
                self.auto_selected = false;
                self.hovered = None;
                self.settle();
            }
            Some(Action::OpenRun(at_ms)) => {
                self.selected_run = Some(at_ms);
                self.hovered = None;
                self.settle();
            }
            Some(Action::TodoNew) => self.begin_edit(None),
            Some(Action::TodoField) => {}
            Some(Action::TodoEdit(id)) => self.begin_edit(Some(id)),
            Some(Action::TodoToggle(id)) => {
                todos::toggle(&mut self.todos, id);
                todos::save(&self.todos);
            }
            Some(Action::TodoDelete(id)) => {
                todos::delete(&mut self.todos, id);
                todos::save(&self.todos);
                self.hovered = None;
            }
            Some(Action::ShelfOpen(path)) => {
                if was_pressed {
                    shell::open(&path);
                }
            }
            Some(Action::ShelfRemove(path)) => {
                self.shelf.retain(|item| item.path != path);
                shelf::save(&self.shelf);
                self.hovered = None;
            }
            Some(Action::DismissReminder) => {
                if let Some(event) = self.reminder.take() {
                    self.dismissed.push((event.title, event.start_ms));
                }
                self.hovered = None;
                self.settle();
            }
            Some(Action::ShelfClear) => {
                self.shelf.clear();
                shelf::save(&self.shelf);
                self.hovered = None;
            }
            Some(Action::CalendarConnect) => drop(self.calendar_tx.send(calendar::Command::Connect)),
            Some(Action::OpenUrl(url)) => shell::open(&url),
            Some(Action::OpenFolder) => {
                let _ = std::fs::create_dir_all(config::dir());
                shell::open(&config::dir().to_string_lossy());
            }
            Some(Action::Jump(agent, id)) => self.jump(agent, &id),
            Some(Action::JumpRun(at_ms)) => self.jump_to_run(at_ms),
            Some(Action::Back) => {
                self.selected = None;
                self.selected_run = None;
                self.auto_selected = false;
                self.hovered = None;
                self.settle();
            }
            Some(Action::SeekTo(ms)) => drop(self.media_tx.send(Command::Seek(ms))),
            Some(Action::ToggleLyrics) => {
                self.config.lyrics = !self.config.lyrics;
                self.config.save();
                self.settle();
            }
            None => {}
        }
        self.dirty = true;
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
            if id != TIMER_TICK && id != TIMER_FULLSCREEN && id != TIMER_WAVE && id != TIMER_REMINDER {
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
            TIMER_REMINDER => {
                if self.current_reminder() != self.reminder {
                    self.settle();
                }
            }
            TIMER_WAVE => {
                let levels = self.wave_state.lock().map_or([0.0; audio::BARS], |w| *w);
                if levels != self.wave {
                    self.wave = levels;
                    self.dirty = true;
                }
            }
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

/// The finished runs the agents page may show.
fn visible_runs<'a>(runs: &'a [Run], config: &Config, now: u64) -> Vec<&'a Run> {
    if !config.recent {
        return vec![];
    }
    runs.iter()
        .filter(|r| r.is_recent(now))
        .filter(|r| match r.agent {
            Agent::Claude => config.track_claude,
            Agent::Codex => config.track_codex,
        })
        .collect()
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
        WM_MOUSEMOVE => app.mouse_move(wparam, lparam),
        WM_LBUTTONDOWN => app.mouse_down(lparam),
        WM_KEYDOWN => app.key_down(wparam.0 as u32),
        WM_CHAR => app.typed(wparam.0 as u16),
        WM_KILLFOCUS => {
            // Clicking anywhere else saves what you typed.
            if app.edit.is_some() {
                app.commit_edit();
                app.focus_before = HWND::default();
                app.settle();
            }
        }
        shelf::WM_DRAG => app.drag_arrived(wparam.0 == 1),
        shelf::WM_DROPPED => app.take_dropped_files(),
        WM_MOUSELEAVE => app.mouse_leave(),
        WM_LBUTTONUP => app.click(lparam),
        WM_WHEEL => app.wheel(((wparam.0 >> 16) as u16 as i16) < 0),
        WM_RBUTTONUP => menu::open(app, true),
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
                menu::open(app, false);
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
        let _ = OleInitialize(None); // the apartment COM needs, plus drag and drop
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(PCWSTR::null()).expect("own module");
        let class = w!("TwoCodeNotch");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() });
        let style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
        let hwnd = CreateWindowExW(style, class, w!("TwoCode"), WS_POPUP, 0, 0, 10, 10, None, None, Some(instance.into()), None).expect("the notch window");
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);

        let config = Config::load();
        let mut runs = config::load_runs();
        runs.retain(|r| r.is_recent(agents::now_ms()));
        let hub = Arc::new(Mutex::new(Hub::default()));
        if let Ok(mut hub) = hub.lock() {
            hub.recent = runs.clone();
        }
        let media_state = Arc::new(Mutex::new(Media::default()));
        let secret = hooks::token(&config::dir());
        hooks::serve(hub.clone(), secret.clone(), tell_window);
        watcher::start(hub.clone(), tell_window);
        let media_tx = media::start(media_state.clone(), tell_window);
        let lyrics_store = Arc::new(Mutex::new(lyrics::Store::default()));
        let lyrics_tx = lyrics::start(lyrics_store.clone(), tell_window);
        let calendar_state = Arc::new(Mutex::new(calendar::State::default()));
        let calendar_tx = calendar::start(calendar_state.clone(), tell_window);
        let wave_state = Arc::new(Mutex::new([0.0; audio::BARS]));
        let audio_tx = audio::start(wave_state.clone());
        let shelf_dropped = Arc::new(Mutex::new(vec![]));
        shelf::accept_drops(hwnd, shelf_dropped.clone());

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
            selected: None,
            auto_selected: false,
            viewed_agents: false,
            runs,
            selected_run: None,
            runs_scroll: 0,
            page_scroll: 0,
            todos: todos::load(),
            edit: None,
            focus_before: HWND::default(),
            high_surrogate: None,
            shelf: shelf::load(),
            shelf_dropped,
            shelf_receiving: false,
            pressed: None,
            calendar_state,
            calendar: calendar::State::default(),
            calendar_tx,
            wave_state,
            wave: [0.0; audio::BARS],
            audio_tx,
            listening: false,
            lyric_strip: false,
            opened_by_hover: false,
            menu: None,
            menu_holds_open: false,
            reminder: None,
            dismissed: vec![],
            tab_reveal: 0.0,
            tab_position: 0.0,
            tabs_clock: Instant::now(),
            dirty: true,
            tray_message: RegisterWindowMessageW(w!("TaskbarCreated")),
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
        (*app).recreate_graphics();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        shell::add_tray(hwnd);
        SetTimer(Some(hwnd), TIMER_FULLSCREEN, 3000, None);
        SetTimer(Some(hwnd), TIMER_REMINDER, REMINDER_CHECK_MS, None);
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
