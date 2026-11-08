//! What the notch shows in each state, and which parts of it you can click.
use crate::agents::{self, Agent, Pill, PlanState, Run, SessionView, Status};
use crate::audio;
use crate::calendar::{self, Phase};
use crate::gfx::{Align, Gfx, Rect};
use crate::layout::{self, Mode, Shape, Tab, CANVAS_W};
use crate::lyrics::{self, Line};
use crate::media::Media;
use crate::shelf;
use crate::todos::{Edit, Todo};

const WHITE: u32 = 0xFFFFFF;
const MUTED: u32 = 0xA3A7AD;
const FAINT: u32 = 0x6C7076;
const GREEN: u32 = 0x4ADE80;
const AMBER: u32 = 0xFBBF24;
const RED: u32 = 0xF87171;
const BLUE: u32 = 0x60A5FA;
const CLAUDE: u32 = 0xE08A5F;
const CODEX: u32 = 0x7DD3C0;
const PAD: f32 = 16.0;
/// Scrollbars sit this far in from the page's right edge, clear of the tab arrow beside it.
const SCROLLBAR_INSET: f32 = 8.0;
/// Myna UI Icons, MIT: see assets/LICENSE-mynaui-icons.txt.
const AIRPODS_SVG: &str = include_str!("../assets/airpods.svg");
const MUSIC_W: f32 = 250.0;
pub const RUN_ROWS: usize = 3;
pub const TODO_ROWS: usize = 3;
pub const EVENT_ROWS: usize = 3;
pub const SHELF_COLUMNS: usize = 4;

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    Previous,
    Toggle,
    Next,
    ToggleLyrics,
    ToggleGlow,
    Seek { x: f32, w: f32, duration_ms: u64 },
    SeekTo(u64),
    Allow(Agent, String),
    Deny(Agent, String),
    Tab(Tab),
    /// The arrows beside the tab row: true goes to the next tab.
    TabStep(bool),
    Open(Agent, String),
    Back,
    Jump(Agent, String),
    OpenRun(u64),
    JumpRun(u64),
    TodoNew,
    TodoField,
    TodoToggle(u64),
    TodoEdit(u64),
    TodoDelete(u64),
    ShelfOpen(String),
    ShelfRemove(String),
    ShelfClear,
    DismissReminder,
    CalendarConnect,
    OpenUrl(String),
    OpenFolder,
}

pub struct Scene<'a> {
    pub shape: Shape,
    pub mode: Mode,
    pub open_h: f32,
    pub tabs: &'a [Tab],
    pub tab: Tab,
    pub media: &'a Media,
    pub lyrics_on: bool,
    pub glow_on: bool,
    pub lyric_lines: &'a [Line],
    pub sessions: &'a [SessionView],
    pub selected: Option<&'a SessionView>,
    pub pill: Pill,
    pub runs: &'a [&'a Run],
    pub selected_run: Option<&'a Run>,
    pub runs_scroll: usize,
    pub following: (bool, bool),
    pub todos: &'a [Todo],
    pub edit: Option<&'a Edit>,
    pub shelf: &'a [shelf::Item],
    pub shelf_receiving: bool,
    pub calendar: &'a calendar::State,
    pub page_scroll: usize,
    pub wave: &'a audio::Levels,
    pub reminder: Option<&'a str>,
    pub tab_reveal: f32,
    pub tab_position: f32,
    pub now_ms: u64,
    pub peek: Option<&'a str>,
    pub peek_status: Status,
    pub device: Option<&'a crate::bluetooth::Notice>,
    pub hovered: Option<&'a Action>,
}

fn status_color(status: Status) -> u32 {
    match status {
        Status::Waiting => AMBER,
        Status::Error => RED,
        Status::Running => BLUE,
        Status::Done => GREEN,
        Status::Idle => FAINT,
    }
}

pub fn draw(g: &mut Gfx, s: &Scene, hits: &mut Vec<(Rect, Action)>) {
    hits.clear();
    let left = (CANVAS_W - s.shape.w) / 2.0;
    let area = Rect { x: left, y: s.shape.y, w: s.shape.w, h: s.shape.h };
    // Bigger shapes grow out of the screen edge: their top corners slide above it as the gap closes.
    let lift = s.shape.radius * (1.0 - s.shape.y / layout::shape_for(Mode::Rest).y).clamp(0.0, 1.0);
    let body = Rect { x: left, y: s.shape.y - lift, w: s.shape.w, h: s.shape.h + lift };
    let at_rest = s.mode == Mode::Rest && s.shape.h < 12.0;
    let fill = match s.pill {
        Pill::Working if at_rest => BLUE,
        Pill::Finished if at_rest => GREEN,
        Pill::Failed if at_rest => RED,
        _ => 0x000000,
    };
    g.fill(body, s.shape.radius, fill, 1.0);
    let open = layout::content_alpha(s.shape.h, s.open_h);
    if s.mode == Mode::Open && open > 0.0 && s.tab == Tab::Media && s.media.present {
        g.tint(body, s.shape.radius, open);
    }
    g.clip(area);
    match s.mode {
        Mode::Open if open > 0.0 => draw_open(g, s, area, open, hits),
        Mode::Peek if s.device.is_some() => draw_device(g, s, area),
        Mode::Peek if s.shape.h > 22.0 => draw_peek(g, s, area),
        Mode::Lyric if s.shape.h > 18.0 => match s.reminder {
            Some(text) => draw_reminder(g, s, text, area, hits),
            None => draw_lyric_strip(g, s, area),
        },
        _ => {}
    }
    g.unclip();
}

/// Headphones connecting or leaving: what they are, and their battery when Windows knows it.
fn draw_device(g: &mut Gfx, s: &Scene, a: Rect) {
    let Some(d) = s.device else { return };
    let alpha = layout::content_alpha(s.shape.h, layout::device_shape().h);
    if alpha <= 0.0 {
        return;
    }
    let (cx, cy) = (a.x + 32.0, a.h / 2.0);
    g.circle(cx, cy, 17.0, WHITE, 0.1 * alpha);
    let tint = if d.connected { WHITE } else { FAINT };
    if d.name.to_lowercase().contains("airpods") {
        g.svg(AIRPODS_SVG, 24.0, Rect { x: cx - 11.0, y: cy - 11.0, w: 22.0, h: 22.0 }, tint, alpha);
    } else {
        draw_headphones(g, cx, cy, tint, alpha);
    }
    let tx = a.x + 58.0;
    let right = if d.battery.is_some() { 74.0 } else { 16.0 };
    let tw = a.x + a.w - right - tx;
    g.text(&d.name, Rect { x: tx, y: cy - 19.0, w: tw, h: 20.0 }, 13.5, true, Align::Left, WHITE, alpha);
    let (state, hex) = if d.connected { ("Connected", GREEN) } else { ("Disconnected", MUTED) };
    g.text(state, Rect { x: tx, y: cy + 1.0, w: tw, h: 18.0 }, 11.5, false, Align::Left, hex, alpha);
    if let Some(level) = d.battery {
        let body = Rect { x: a.x + a.w - 70.0, y: cy - 6.5, w: 25.0, h: 13.0 };
        let fill = if level <= 20 { RED } else { GREEN };
        g.fill(body, 3.5, WHITE, 0.35 * alpha);
        g.fill(Rect { x: body.x + 1.5, y: body.y + 1.5, w: body.w - 3.0, h: body.h - 3.0 }, 2.5, 0x000000, alpha);
        g.fill(Rect { x: body.x + 2.5, y: body.y + 2.5, w: (body.w - 5.0) * level as f32 / 100.0, h: body.h - 5.0 }, 1.5, fill, alpha);
        g.fill(Rect { x: body.x + body.w + 1.0, y: cy - 2.5, w: 2.0, h: 5.0 }, 1.0, WHITE, 0.35 * alpha);
        g.text(&format!("{level}%"), Rect { x: body.x + body.w + 5.0, y: cy - 9.0, w: 40.0, h: 18.0 }, 11.5, true, Align::Left, WHITE, alpha);
    }
}

/// A headband over two ear cups, as one outline for the band and two rounded cups.
fn draw_headphones(g: &mut Gfx, cx: f32, cy: f32, hex: u32, alpha: f32) {
    const STEPS: usize = 14;
    let (outer, inner) = (9.0, 7.2);
    let mut band = vec![];
    for i in 0..=STEPS {
        let angle = std::f32::consts::PI * (1.0 + i as f32 / STEPS as f32);
        band.push((cx + outer * angle.cos(), cy + 1.0 + outer * angle.sin()));
    }
    for i in (0..=STEPS).rev() {
        let angle = std::f32::consts::PI * (1.0 + i as f32 / STEPS as f32);
        band.push((cx + inner * angle.cos(), cy + 1.0 + inner * angle.sin()));
    }
    g.polygon(&band, hex, alpha);
    g.fill(Rect { x: cx - 10.0, y: cy, w: 5.5, h: 9.0 }, 2.0, hex, alpha);
    g.fill(Rect { x: cx + 4.5, y: cy, w: 5.5, h: 9.0 }, 2.0, hex, alpha);
}

fn draw_peek(g: &mut Gfx, s: &Scene, a: Rect) {
    let Some(text) = s.peek else { return };
    g.circle(a.x + 20.0, a.h / 2.0, 4.0, status_color(s.peek_status), 1.0);
    g.text(text, Rect { x: a.x + 32.0, y: 0.0, w: a.w - 46.0, h: a.h }, 12.0, true, Align::Left, WHITE, 1.0);
}

/// A meeting about to start, with an × that puts the pill back.
fn draw_reminder(g: &mut Gfx, s: &Scene, text: &str, a: Rect, hits: &mut Vec<(Rect, Action)>) {
    g.circle(a.x + 14.0, a.h / 2.0, 3.5, AMBER, 1.0);
    g.text(text, Rect { x: a.x + 24.0, y: 0.0, w: a.w - 24.0 - layout::STRIP_CLOSE_W, h: a.h }, layout::LYRIC_TEXT, true, Align::Left, WHITE, 1.0);
    let close = Rect { x: a.x + a.w - layout::STRIP_CLOSE_W, y: 0.0, w: layout::STRIP_CLOSE_W, h: a.h };
    let over = is_hovered(s, &Action::DismissReminder);
    if over {
        g.circle(close.x + close.w / 2.0, a.h / 2.0, 9.0, WHITE, 0.18);
    }
    g.text("×", close, 15.0, false, Align::Center, if over { WHITE } else { MUTED }, 1.0);
    hits.push((close, Action::DismissReminder));
}

/// The line being sung. The pill's color is hidden behind this, so agent news shows as a small dot.
fn draw_lyric_strip(g: &mut Gfx, s: &Scene, a: Rect) {
    let dot = match s.pill {
        Pill::Working => Some(BLUE),
        Pill::Finished => Some(GREEN),
        Pill::Failed => Some(RED),
        _ => None,
    };
    if let Some(hex) = dot {
        g.circle(a.x + 14.0, a.h / 2.0, 3.5, hex, 1.0);
    }
    let text = lyrics::current_line(s.lyric_lines, s.media.position_now()).map_or("♪", |i| s.lyric_lines[i].text.as_str());
    g.text(text, Rect { x: a.x + 24.0, y: 0.0, w: a.w - 48.0, h: a.h }, layout::LYRIC_TEXT, true, Align::Center, WHITE, 1.0);
}

/// Bars growing from their middle, each one a band of pitches in what is playing right now.
fn draw_wave(g: &mut Gfx, levels: &audio::Levels, r: Rect, alpha: f32) {
    let (bar, gap) = (3.0, 2.0);
    let total = audio::BARS as f32 * (bar + gap) - gap;
    let mid = r.y + r.h / 2.0;
    for (i, level) in levels.iter().enumerate() {
        let h = (level * r.h).max(bar);
        let x = r.x + r.w - total + i as f32 * (bar + gap);
        g.fill(Rect { x, y: mid - h / 2.0, w: bar, h }, bar / 2.0, WHITE, (0.45 + 0.5 * level) * alpha);
    }
}

fn draw_open(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    // The page sits inside the gutters the arrows live in.
    let page = Rect { x: a.x + layout::ARROW_GUTTER, w: a.w - layout::ARROW_GUTTER * 2.0, ..a };
    match s.tab {
        Tab::Media => draw_music(g, s, page, alpha, hits),
        Tab::Agents => draw_agents(g, s, page, alpha, hits),
        Tab::Calendar => draw_calendar(g, s, page, alpha, hits),
        Tab::Todos => draw_todos(g, s, page, alpha, hits),
        Tab::Shelf => draw_shelf(g, s, page, alpha, hits),
    }
    if s.tabs.len() > 1 {
        draw_tab_picker(g, s, a, alpha, hits);
    }
}

/// One row that never swaps layouts: pointing at it spreads the dots apart and they become icons,
/// while one soft marker slides between tabs and grows from a small bar into a highlight behind the icon.
fn draw_tab_picker(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let mix = |from: f32, to: f32| from + (to - from) * s.tab_reveal;
    let (mid, cy, count) = (a.x + a.w / 2.0, s.open_h - layout::TAB_ROW_FROM_BOTTOM, s.tabs.len());
    let spacing = mix(layout::TAB_DOT_SPACING, layout::TAB_ICON_SPACING);
    let (marker_w, marker_h) = (mix(14.0, 28.0), mix(5.0, 19.0));
    let marker_x = layout::tab_x(mid, s.tab_position, count, spacing);
    g.fill(Rect { x: marker_x - marker_w / 2.0, y: cy - marker_h / 2.0, w: marker_w, h: marker_h }, marker_h / 2.0, WHITE, mix(0.95, 0.16) * alpha);
    for (i, tab) in s.tabs.iter().enumerate() {
        let x = layout::tab_x(mid, i as f32, count, spacing);
        let action = Action::Tab(*tab);
        let chosen = *tab == s.tab;
        let status = if *tab == Tab::Agents && !chosen { layout::light(s.sessions.iter().map(|v| v.status)) } else { None };
        // A dot is hidden under the marker when its tab is chosen, so only the others show.
        let closeness = (s.tab_position - i as f32).abs().min(1.0);
        match status {
            Some(status) => g.circle(x, cy, 2.75, status_color(status), (1.0 - s.tab_reveal) * alpha),
            None => g.circle(x, cy, 2.75, WHITE, 0.4 * closeness * (1.0 - s.tab_reveal) * alpha),
        }
        if s.tab_reveal > 0.01 {
            let shade = if chosen || is_hovered(s, &action) { 1.0 } else { 0.55 };
            g.scaled(x, cy, mix(0.55, 0.82));
            draw_tab_icon(g, *tab, x, cy + 0.5, shade * s.tab_reveal * alpha);
            g.unscaled();
            if let Some(status) = status {
                g.circle(x + 8.0, cy - 6.0, 2.5, status_color(status), s.tab_reveal * alpha);
            }
        }
        // The same targets throughout, so the row can't flicker as it moves.
        let hit_x = layout::tab_x(mid, i as f32, count, layout::TAB_ICON_SPACING);
        let hit_top = s.open_h - layout::TAB_ROW_FROM_BOTTOM * 2.0 - 1.0;
        hits.push((Rect { x: hit_x - layout::TAB_ICON_SPACING / 2.0, y: hit_top, w: layout::TAB_ICON_SPACING, h: s.open_h - hit_top }, action));
    }
    // An arrow on each side, halfway down the page, drawn like the icons: quiet until pointed at.
    // Its target is a slim strip along the edge, so it never takes clicks meant for the page.
    let ay = (s.open_h - layout::TAB_ROW_FROM_BOTTOM * 2.0) / 2.0;
    for forward in [false, true] {
        let action = Action::TabStep(forward);
        let x = if forward { a.x + a.w - layout::TAB_ARROW_INSET } else { a.x + layout::TAB_ARROW_INSET };
        let over = is_hovered(s, &action);
        g.polygon(&chevron(x, ay, forward), WHITE, if over { 0.85 } else { 0.28 } * alpha);
        let edge = if forward { x - 7.0 } else { a.x };
        hits.push((Rect { x: edge, y: ay - 18.0, w: layout::TAB_ARROW_INSET + 7.0, h: 36.0 }, action));
    }
}

/// A shallow "<" or ">" as one thin outline: wide open, so it reads as a hint rather than a button.
fn chevron(x: f32, y: f32, forward: bool) -> Vec<(f32, f32)> {
    let side = if forward { -1.0 } else { 1.0 };
    let (reach, height, thick): (f32, f32, f32) = (1.8, 4.6, 1.25);
    // Shifting an arm sideways by `shift` gives it this thickness measured across it.
    let shift = thick * (4.0 * reach * reach + height * height).sqrt() / height;
    [(reach, -height), (reach + shift, -height), (-reach + shift, 0.0), (reach + shift, height), (reach, height), (-reach, 0.0)]
        .iter()
        .map(|(dx, dy)| (x + dx * side, y + dy))
        .collect()
}

fn draw_tab_icon(g: &mut Gfx, tab: Tab, cx: f32, cy: f32, alpha: f32) {
    match tab {
        Tab::Media => {
            g.circle(cx - 2.5, cy + 5.0, 3.0, WHITE, alpha);
            g.fill(Rect { x: cx - 0.2, y: cy - 7.0, w: 1.9, h: 12.0 }, 0.5, WHITE, alpha);
            g.polygon(&[(cx + 1.0, cy - 7.0), (cx + 6.5, cy - 4.5), (cx + 6.5, cy - 1.5), (cx + 1.0, cy - 4.0)], WHITE, alpha);
        }
        Tab::Agents => {
            g.fill(Rect { x: cx - 0.8, y: cy - 8.5, w: 1.6, h: 3.5 }, 0.5, WHITE, alpha);
            g.circle(cx, cy - 8.5, 1.4, WHITE, alpha);
            g.fill(Rect { x: cx - 7.0, y: cy - 5.5, w: 14.0, h: 11.5 }, 3.5, WHITE, alpha);
            g.fill(Rect { x: cx - 9.0, y: cy - 2.0, w: 1.6, h: 5.0 }, 0.8, WHITE, alpha);
            g.fill(Rect { x: cx + 7.4, y: cy - 2.0, w: 1.6, h: 5.0 }, 0.8, WHITE, alpha);
            g.circle(cx - 3.0, cy, 1.7, 0x000000, alpha);
            g.circle(cx + 3.0, cy, 1.7, 0x000000, alpha);
        }
        Tab::Calendar => {
            g.fill(Rect { x: cx - 7.0, y: cy - 6.0, w: 14.0, h: 13.0 }, 2.5, WHITE, alpha);
            g.fill(Rect { x: cx - 5.5, y: cy - 2.0, w: 11.0, h: 7.5 }, 1.0, 0x000000, alpha);
            g.fill(Rect { x: cx - 4.0, y: cy - 8.0, w: 1.8, h: 3.5 }, 0.8, WHITE, alpha);
            g.fill(Rect { x: cx + 2.2, y: cy - 8.0, w: 1.8, h: 3.5 }, 0.8, WHITE, alpha);
            g.fill(Rect { x: cx - 3.5, y: cy, w: 3.0, h: 3.0 }, 0.5, WHITE, alpha);
        }
        Tab::Todos => {
            for (row, wide) in [(-5.0, 9.0), (0.0, 7.0), (5.0, 9.0)] {
                g.circle(cx - 6.0, cy + row, 1.5, WHITE, alpha);
                g.fill(Rect { x: cx - 2.5, y: cy + row - 0.9, w: wide, h: 1.8 }, 0.9, WHITE, alpha);
            }
        }
        Tab::Shelf => {
            // A tray with an arrow dropping into it.
            g.fill(Rect { x: cx - 8.0, y: cy + 1.0, w: 2.0, h: 6.5 }, 1.0, WHITE, alpha);
            g.fill(Rect { x: cx + 6.0, y: cy + 1.0, w: 2.0, h: 6.5 }, 1.0, WHITE, alpha);
            g.fill(Rect { x: cx - 8.0, y: cy + 5.5, w: 16.0, h: 2.0 }, 1.0, WHITE, alpha);
            g.fill(Rect { x: cx - 1.0, y: cy - 8.0, w: 2.0, h: 8.0 }, 0.5, WHITE, alpha);
            g.polygon(&[(cx - 4.0, cy - 2.0), (cx + 4.0, cy - 2.0), (cx, cy + 3.0)], WHITE, alpha);
        }
    }
}

fn is_hovered(s: &Scene, action: &Action) -> bool {
    match (s.hovered, action) {
        (Some(Action::Seek { .. }), Action::Seek { .. }) => true,
        (Some(a), b) => a == b,
        _ => false,
    }
}

fn draw_music(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let m = s.media;
    if !m.present {
        draw_empty_player(g, s, a, alpha);
        return;
    }
    let split = s.lyrics_on && !s.lyric_lines.is_empty();
    let x = a.x + PAD + 4.0;
    let w = if split { MUSIC_W } else { a.w - (PAD + 4.0) * 2.0 };

    let art = Rect { x, y: 10.0, w: 54.0, h: 54.0 };
    if g.has_art() {
        g.draw_art(art, 10.0, alpha);
    } else {
        g.fill(art, 10.0, WHITE, 0.08 * alpha);
        g.polygon(&[(art.x + 21.0, art.y + 18.0), (art.x + 37.0, art.y + 27.0), (art.x + 21.0, art.y + 36.0)], WHITE, 0.45 * alpha);
    }
    let tx = art.x + art.w + 12.0;
    let wave_w = audio::BARS as f32 * 5.0 + 6.0;
    draw_wave(g, s.wave, Rect { x: x + w - wave_w, y: 18.0, w: wave_w, h: 36.0 }, alpha);
    let tw = x + w - tx - wave_w;
    let title = if m.title.is_empty() { "Unknown" } else { &m.title };
    g.text(title, Rect { x: tx, y: 16.0, w: tw, h: 22.0 }, 14.5, true, Align::Left, WHITE, alpha);
    g.text(&m.artist, Rect { x: tx, y: 38.0, w: tw, h: 18.0 }, 12.0, false, Align::Left, MUTED, alpha);

    // time, progress, time left
    let cy = 78.0;
    let now = m.position_now();
    if m.duration_ms > 0 {
        let label = 40.0;
        let bar = Rect { x: x + label, y: cy - 1.5, w: w - label * 2.0, h: 3.0 };
        let seek = Action::Seek { x: bar.x, w: bar.w, duration_ms: m.duration_ms };
        let grow = if m.can_seek && is_hovered(s, &seek) { 1.5 } else { 0.0 };
        let fraction = (now as f32 / m.duration_ms as f32).clamp(0.0, 1.0);
        let track = Rect { y: bar.y - grow / 2.0, h: bar.h + grow, ..bar };
        g.fill(track, 2.0, WHITE, 0.22 * alpha);
        g.fill(Rect { w: (bar.w * fraction).max(3.0), ..track }, 2.0, WHITE, 0.95 * alpha);
        if m.can_seek {
            hits.push((Rect { x: bar.x, y: cy - 8.0, w: bar.w, h: 16.0 }, seek));
        }
        g.text(&layout::clock(now), Rect { x, y: cy - 9.0, w: label - 6.0, h: 18.0 }, 11.0, false, Align::Left, WHITE, alpha);
        let left = format!("-{}", layout::clock(m.duration_ms.saturating_sub(now)));
        g.text(&left, Rect { x: x + w - label, y: cy - 9.0, w: label, h: 18.0 }, 11.0, false, Align::Right, WHITE, alpha);
    }

    // controls, with the lyrics button on the left like the speech bubble in the references
    let cy = 104.0;
    let cx = x + w / 2.0;
    let bubble = Rect { x: x - 2.0, y: cy - 14.0, w: 28.0, h: 28.0 };
    if is_hovered(s, &Action::ToggleLyrics) {
        g.fill(bubble, 14.0, WHITE, 0.12 * alpha);
    }
    let shade = if s.lyrics_on { 1.0 } else { 0.45 };
    draw_bubble(g, bubble.x + 14.0, cy, shade * alpha);
    hits.push((bubble, Action::ToggleLyrics));
    // The screen glow's switch mirrors the lyrics button on the right.
    let lamp = Rect { x: x + w - 26.0, y: cy - 14.0, w: 28.0, h: 28.0 };
    if is_hovered(s, &Action::ToggleGlow) {
        g.fill(lamp, 14.0, WHITE, 0.12 * alpha);
    }
    draw_glow_icon(g, lamp.x + 14.0, cy, if s.glow_on { 1.0 } else { 0.45 } * alpha);
    hits.push((lamp, Action::ToggleGlow));
    for (offset, action) in [(-40.0, Action::Previous), (0.0, Action::Toggle), (40.0, Action::Next)] {
        let box_r = Rect { x: cx + offset - 14.0, y: cy - 14.0, w: 28.0, h: 28.0 };
        if is_hovered(s, &action) {
            g.fill(box_r, 14.0, WHITE, 0.12 * alpha);
        }
        draw_transport(g, cx + offset, cy, &action, m.playing, alpha);
        hits.push((box_r, action));
    }

    if split && a.w > MUSIC_W + 120.0 {
        let lx = x + w + 16.0;
        g.fill(Rect { x: lx - 8.0, y: 14.0, w: 1.0, h: 100.0 }, 0.0, WHITE, 0.1 * alpha);
        draw_lyrics(g, s, Rect { x: lx, y: 8.0, w: a.x + a.w - lx - PAD, h: 112.0 }, now, alpha, hits);
    }
}

/// A small sun: a dot with eight rays.
fn draw_glow_icon(g: &mut Gfx, cx: f32, cy: f32, alpha: f32) {
    g.circle(cx, cy, 3.4, WHITE, alpha);
    for k in 0..8 {
        let angle = k as f32 * std::f32::consts::FRAC_PI_4;
        let (dx, dy) = (angle.cos(), angle.sin());
        let (nx, ny) = (-dy * 0.75, dx * 0.75);
        let (near, far) = (5.6, 8.2);
        g.polygon(
            &[
                (cx + dx * near + nx, cy + dy * near + ny),
                (cx + dx * far + nx, cy + dy * far + ny),
                (cx + dx * far - nx, cy + dy * far - ny),
                (cx + dx * near - nx, cy + dy * near - ny),
            ],
            WHITE,
            alpha,
        );
    }
}

/// A speech bubble as one outline, tail included, so its parts never show as separate shapes.
fn bubble_outline(x: f32, y: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 6;
    let (left, right, top, bottom, r) = (x - 8.5, x + 8.5, y - 7.0, y + 4.5, 4.0);
    let mut points = vec![];
    // Clockwise from the top left: each corner is an arc around its center, starting at this angle.
    for (cx, cy, start) in [(left + r, top + r, 180.0f32), (right - r, top + r, 270.0), (right - r, bottom - r, 0.0)] {
        for step in 0..=STEPS {
            let angle = (start + 90.0 * step as f32 / STEPS as f32).to_radians();
            points.push((cx + r * angle.cos(), cy + r * angle.sin()));
        }
    }
    // The bottom edge flows into the tail, which ends in a soft point.
    points.extend([(x + 0.5, bottom), (x - 3.2, y + 7.9), (x - 4.2, y + 8.3), (x - 4.7, y + 7.5), (x - 4.2, bottom)]);
    for step in 0..=STEPS {
        let angle = (90.0 + 90.0 * step as f32 / STEPS as f32).to_radians();
        points.push((left + r + r * angle.cos(), bottom - r + r * angle.sin()));
    }
    points
}

fn draw_bubble(g: &mut Gfx, x: f32, y: f32, alpha: f32) {
    g.polygon(&bubble_outline(x, y), WHITE, alpha);
}

fn draw_transport(g: &mut Gfx, px: f32, cy: f32, action: &Action, playing: bool, alpha: f32) {
    match action {
        Action::Previous => {
            g.polygon(&[(px, cy - 6.0), (px, cy + 6.0), (px - 8.0, cy)], WHITE, alpha);
            g.polygon(&[(px + 8.0, cy - 6.0), (px + 8.0, cy + 6.0), (px, cy)], WHITE, alpha);
        }
        Action::Next => {
            g.polygon(&[(px, cy - 6.0), (px, cy + 6.0), (px + 8.0, cy)], WHITE, alpha);
            g.polygon(&[(px - 8.0, cy - 6.0), (px - 8.0, cy + 6.0), (px, cy)], WHITE, alpha);
        }
        _ if playing => {
            g.fill(Rect { x: px - 5.0, y: cy - 7.0, w: 3.5, h: 14.0 }, 1.0, WHITE, alpha);
            g.fill(Rect { x: px + 1.5, y: cy - 7.0, w: 3.5, h: 14.0 }, 1.0, WHITE, alpha);
        }
        _ => g.polygon(&[(px - 5.0, cy - 8.0), (px - 5.0, cy + 8.0), (px + 8.0, cy)], WHITE, alpha),
    }
}

/// The player at rest: the same layout as a playing song, with nothing to press.
fn draw_empty_player(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32) {
    let x = a.x + PAD + 4.0;
    let w = a.w - (PAD + 4.0) * 2.0;
    let art = Rect { x, y: 10.0, w: 54.0, h: 54.0 };
    g.fill(art, 10.0, WHITE, 0.08 * alpha);
    draw_tab_icon(g, Tab::Media, art.x + 26.0, art.y + 28.0, 0.45 * alpha);
    let tx = art.x + art.w + 12.0;
    let tw = x + w - tx;
    // Sound from an app that doesn't report what it's playing still moves the bars.
    if s.wave.iter().any(|level| *level > 0.0) {
        let wave_w = audio::BARS as f32 * 5.0 + 6.0;
        draw_wave(g, s.wave, Rect { x: x + w - wave_w, y: 18.0, w: wave_w, h: 36.0 }, alpha);
    }
    g.text("Nothing playing", Rect { x: tx, y: 16.0, w: tw, h: 22.0 }, 14.5, true, Align::Left, WHITE, alpha);
    g.text("Play something in any app", Rect { x: tx, y: 38.0, w: tw, h: 18.0 }, 12.0, false, Align::Left, MUTED, alpha);

    let cy = 78.0;
    let label = 40.0;
    g.fill(Rect { x: x + label, y: cy - 1.5, w: w - label * 2.0, h: 3.0 }, 2.0, WHITE, 0.14 * alpha);
    g.text("0:00", Rect { x, y: cy - 9.0, w: label - 6.0, h: 18.0 }, 11.0, false, Align::Left, FAINT, alpha);
    g.text("--:--", Rect { x: x + w - label, y: cy - 9.0, w: label, h: 18.0 }, 11.0, false, Align::Right, FAINT, alpha);

    let cy = 104.0;
    let cx = x + w / 2.0;
    draw_bubble(g, x + 12.0, cy, 0.2 * alpha);
    for (offset, action) in [(-40.0, Action::Previous), (0.0, Action::Toggle), (40.0, Action::Next)] {
        draw_transport(g, cx + offset, cy, &action, false, 0.28 * alpha);
    }
}

/// The line being sung in the middle, the ones around it dimmed. Click a line to jump there.
fn draw_lyrics(g: &mut Gfx, s: &Scene, r: Rect, now: u64, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let lines = s.lyric_lines;
    let current = lyrics::current_line(lines, now);
    let middle = Rect { x: r.x, y: r.y + 28.0, w: r.w, h: 56.0 };
    let side = |y: f32| Rect { x: r.x, y, w: r.w, h: 24.0 };
    let mut other = |g: &mut Gfx, line: Option<&Line>, area: Rect| {
        let Some(line) = line else { return };
        let action = Action::SeekTo(line.at_ms);
        let shade = if is_hovered(s, &action) { 0.75 } else { 0.38 };
        g.text(&line.text, area, 12.5, true, Align::Left, WHITE, shade * alpha);
        hits.push((area, action));
    };
    other(g, current.and_then(|i| i.checked_sub(1)).and_then(|i| lines.get(i)), side(r.y + 2.0));
    other(g, lines.get(current.map_or(0, |i| i + 1)), side(r.y + 86.0));
    let text = current.map_or("♪", |i| lines[i].text.as_str());
    g.text_wrapped(text, middle, 15.0, true, Align::Left, WHITE, alpha);
}

fn brand(agent: Agent) -> (&'static str, u32) {
    match agent {
        Agent::Claude => ("Claude", CLAUDE),
        Agent::Codex => ("Codex", CODEX),
    }
}

fn state_color(v: &SessionView) -> u32 {
    if v.is_quiet() {
        AMBER
    } else {
        status_color(v.status)
    }
}

fn draw_agents(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    if let Some(v) = s.selected {
        draw_session(g, s, v, a, alpha, hits);
    } else if let Some(run) = s.selected_run {
        draw_run(g, s, run, a, alpha, hits);
    } else if agents::any_live(s.sessions) {
        draw_list(g, s, a, alpha, hits);
    } else if !s.runs.is_empty() {
        draw_runs(g, s, a, alpha, hits);
    } else if !s.sessions.is_empty() {
        draw_list(g, s, a, alpha, hits);
    } else {
        draw_no_agents(g, s, a, alpha);
    }
}

/// The page at rest: which agents it is listening for, in the same layout as the player.
fn draw_no_agents(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32) {
    let x = a.x + PAD + 4.0;
    let w = a.w - (PAD + 4.0) * 2.0;
    let tile = Rect { x, y: 10.0, w: 54.0, h: 54.0 };
    g.fill(tile, 10.0, WHITE, 0.08 * alpha);
    draw_tab_icon(g, Tab::Agents, tile.x + 27.0, tile.y + 29.0, 0.45 * alpha);
    let tx = tile.x + tile.w + 12.0;
    let tw = x + w - tx;
    let hint = if s.following == (false, false) { "Follow one from the right-click menu" } else { "Runs appear here when they start" };
    g.text("No agents running", Rect { x: tx, y: 16.0, w: tw, h: 22.0 }, 14.5, true, Align::Left, WHITE, alpha);
    g.text(hint, Rect { x: tx, y: 38.0, w: tw, h: 18.0 }, 12.0, false, Align::Left, MUTED, alpha);

    let half = (w - 8.0) / 2.0;
    for (i, (agent, on)) in [(Agent::Claude, s.following.0), (Agent::Codex, s.following.1)].into_iter().enumerate() {
        let r = Rect { x: x + i as f32 * (half + 8.0), y: 76.0, w: half, h: 28.0 };
        let (name, tint) = brand(agent);
        g.fill(r, 14.0, WHITE, 0.06 * alpha);
        g.circle(r.x + 14.0, r.y + 14.0, 3.5, if on { tint } else { FAINT }, alpha);
        g.text(name, Rect { x: r.x + 24.0, y: r.y, w: 48.0, h: r.h }, 11.5, true, Align::Left, if on { WHITE } else { FAINT }, alpha);
        g.text(if on { "Following" } else { "Off" }, Rect { x: r.x + r.w - 72.0, y: r.y, w: 60.0, h: r.h }, 10.5, false, Align::Right, FAINT, alpha);
    }
}

/// The session that matters most gets the room; the others stay as small rows.
fn draw_list(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let v = &s.sessions[0];
    let (x, w) = (a.x + 18.0, a.w - 36.0);
    let action = Action::Open(v.agent, v.id.clone());
    let main = Rect { x: a.x + 8.0, y: 4.0, w: a.w - 16.0, h: 70.0 };
    if is_hovered(s, &action) {
        g.fill(main, 12.0, WHITE, 0.06 * alpha);
    }
    hits.push((main, action));
    let (name, tint) = brand(v.agent);
    g.circle(x + 4.0, 17.0, 4.0, state_color(v), alpha);
    g.text(name, Rect { x: x + 14.0, y: 8.0, w: 44.0, h: 18.0 }, 11.5, true, Align::Left, tint, alpha);
    let status = if v.elapsed_s > 0 { format!("{} · {}", v.status_text(), layout::elapsed(v.elapsed_s)) } else { v.status_text() };
    g.text(&v.project, Rect { x: x + 58.0, y: 8.0, w: w - 58.0 - 136.0, h: 18.0 }, 12.0, true, Align::Left, WHITE, alpha);
    g.text(&status, Rect { x: x + w - 136.0, y: 8.0, w: 136.0, h: 18.0 }, 11.0, true, Align::Right, state_color(v), alpha);
    g.text(&v.prompt, Rect { x, y: 26.0, w, h: 16.0 }, 11.0, false, Align::Left, MUTED, alpha);
    let finished = v.status == Status::Done;
    let headline = if finished { v.summary() } else { v.progress_line().unwrap_or_else(|| v.detail.clone()) };
    g.text(&headline, Rect { x, y: 43.0, w, h: 18.0 }, 12.5, true, Align::Left, WHITE, alpha);
    if finished {
        g.text(&v.files.join(", "), Rect { x, y: 60.0, w, h: 15.0 }, 10.5, false, Align::Left, FAINT, alpha);
    } else if let Some(fraction) = v.progress() {
        let bar = Rect { x, y: 65.0, w, h: 3.0 };
        g.fill(bar, 1.5, WHITE, 0.16 * alpha);
        g.fill(Rect { w: (w * fraction).max(3.0), ..bar }, 1.5, BLUE, alpha);
    }

    let others = &s.sessions[1..];
    let active: Vec<&SessionView> = others.iter().filter(|o| o.status != Status::Done).collect();
    let done: Vec<&SessionView> = others.iter().filter(|o| o.status == Status::Done).collect();
    let summary_needed = !done.is_empty() || active.len() > 2;
    let room = if summary_needed { 1 } else { 2 };
    let mut y = 78.0;
    for o in active.iter().take(room) {
        draw_small_row(g, s, o, Rect { x: a.x + 8.0, y, w: a.w - 16.0, h: 18.0 }, alpha, hits);
        y += 19.0;
    }
    if summary_needed {
        let more = active.len().saturating_sub(room);
        let text = match (more, done.len()) {
            (0, n) => format!("{n} finished"),
            (m, 0) => format!("+{m} more"),
            (m, n) => format!("+{m} more · {n} finished"),
        };
        let r = Rect { x: a.x + 8.0, y, w: a.w - 16.0, h: 18.0 };
        let target = done.first().copied().or(active.get(room).copied());
        if let Some(t) = target {
            let action = Action::Open(t.agent, t.id.clone());
            if is_hovered(s, &action) {
                g.fill(r, 8.0, WHITE, 0.08 * alpha);
            }
            hits.push((r, action));
        }
        g.text(&text, Rect { x: r.x + 10.0, ..r }, 10.5, false, Align::Left, FAINT, alpha);
    } else if active.is_empty() && v.progress().is_some() && !finished {
        g.text(&v.detail, Rect { x, y: 76.0, w, h: 18.0 }, 11.0, false, Align::Left, MUTED, alpha);
    }
}

fn draw_small_row(g: &mut Gfx, s: &Scene, v: &SessionView, r: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let action = Action::Open(v.agent, v.id.clone());
    if is_hovered(s, &action) {
        g.fill(r, 8.0, WHITE, 0.08 * alpha);
    }
    let (name, tint) = brand(v.agent);
    g.circle(r.x + 14.0, r.y + 9.0, 3.0, state_color(v), alpha);
    g.text(name, Rect { x: r.x + 24.0, y: r.y, w: 42.0, h: r.h }, 10.5, true, Align::Left, tint, alpha);
    g.text(&v.project, Rect { x: r.x + 66.0, y: r.y, w: r.w - 66.0 - 122.0, h: r.h }, 11.0, false, Align::Left, WHITE, alpha);
    g.text(&v.status_text(), Rect { x: r.x + r.w - 118.0, y: r.y, w: 108.0, h: r.h }, 10.5, true, Align::Right, state_color(v), alpha);
    hits.push((r, action));
}

/// A page's resting state: icon tile, a headline and a line under it, as on the player.
fn draw_page_header(g: &mut Gfx, tab: Tab, title: &str, line: &str, a: Rect, alpha: f32) {
    let x = a.x + PAD + 4.0;
    let tile = Rect { x, y: 10.0, w: 54.0, h: 54.0 };
    g.fill(tile, 10.0, WHITE, 0.08 * alpha);
    draw_tab_icon(g, tab, tile.x + 27.0, tile.y + 28.0, 0.45 * alpha);
    let tx = tile.x + tile.w + 12.0;
    let tw = a.x + a.w - PAD - 4.0 - tx;
    g.text(title, Rect { x: tx, y: 16.0, w: tw, h: 22.0 }, 14.5, true, Align::Left, WHITE, alpha);
    g.text(line, Rect { x: tx, y: 38.0, w: tw, h: 18.0 }, 12.0, false, Align::Left, MUTED, alpha);
}

fn draw_pill_button(g: &mut Gfx, s: &Scene, label: &str, r: Rect, action: Action, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    g.fill(r, r.h / 2.0, WHITE, if is_hovered(s, &action) { 0.24 } else { 0.12 } * alpha);
    g.text(label, r, 11.5, true, Align::Center, WHITE, alpha);
    hits.push((r, action));
}

/// One line being typed. Long text slides left so the caret stays visible.
fn draw_field(g: &mut Gfx, edit: &Edit, r: Rect, size: f32, alpha: f32) {
    let caret = g.text_width(&edit.before_caret(), size, false);
    let shift = (caret - (r.w - 4.0)).max(0.0);
    g.clip(r);
    g.text(&edit.text(), Rect { x: r.x - shift, y: r.y, w: 10_000.0, h: r.h }, size, false, Align::Left, WHITE, alpha);
    g.fill(Rect { x: r.x + caret - shift, y: r.y + 3.0, w: 1.3, h: r.h - 6.0 }, 0.0, WHITE, alpha);
    g.unclip();
}

fn draw_todos(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let (x, w) = (a.x + PAD, a.w - PAD * 2.0);
    let input = Rect { x, y: 8.0, w, h: 26.0 };
    let typing_new = s.edit.filter(|e| e.target.is_none());
    g.fill(input, 13.0, WHITE, if typing_new.is_some() { 0.14 } else { 0.08 } * alpha);
    let inner = Rect { x: x + 12.0, y: input.y, w: w - 24.0, h: input.h };
    match typing_new {
        Some(edit) => {
            draw_field(g, edit, inner, 12.0, alpha);
            hits.push((input, Action::TodoField));
        }
        None => {
            g.text("+  Add a todo", inner, 12.0, false, Align::Left, if is_hovered(s, &Action::TodoNew) { WHITE } else { MUTED }, alpha);
            hits.push((input, Action::TodoNew));
        }
    }
    if s.todos.is_empty() {
        g.text("Nothing to do", Rect { x, y: 44.0, w, h: 60.0 }, 12.5, false, Align::Center, FAINT, alpha);
        return;
    }
    let row_h = 25.0;
    let scroll = s.page_scroll.min(s.todos.len().saturating_sub(TODO_ROWS));
    for (i, todo) in s.todos.iter().enumerate().skip(scroll).take(TODO_ROWS) {
        let r = Rect { x, y: 38.0 + (i - scroll) as f32 * row_h, w, h: row_h };
        let (toggle, change, delete) = (Action::TodoToggle(todo.id), Action::TodoEdit(todo.id), Action::TodoDelete(todo.id));
        let over = is_hovered(s, &toggle) || is_hovered(s, &change) || is_hovered(s, &delete);
        if over {
            g.fill(r, 8.0, WHITE, 0.06 * alpha);
        }
        let (cx, cy) = (r.x + 13.0, r.y + r.h / 2.0);
        if todo.done {
            g.circle(cx, cy, 6.5, GREEN, alpha);
            g.polygon(&[(cx - 3.4, cy - 0.2), (cx - 2.2, cy - 1.4), (cx - 0.8, cy + 0.2), (cx + 2.4, cy - 3.0), (cx + 3.6, cy - 1.8), (cx - 0.8, cy + 2.6)], 0x052E16, alpha);
        } else {
            g.circle(cx, cy, 6.5, if is_hovered(s, &toggle) { WHITE } else { MUTED }, alpha);
            g.circle(cx, cy, 5.2, 0x000000, alpha);
        }
        let text_r = Rect { x: r.x + 26.0, y: r.y, w: r.w - 26.0 - 24.0, h: r.h };
        if let Some(edit) = s.edit.filter(|e| e.target == Some(todo.id)) {
            g.fill(Rect { x: text_r.x - 5.0, y: r.y + 2.0, w: text_r.w + 10.0, h: r.h - 4.0 }, 6.0, WHITE, 0.14 * alpha);
            draw_field(g, edit, text_r, 12.0, alpha);
            hits.push((text_r, Action::TodoField));
        } else {
            g.text(&todo.text, text_r, 12.0, false, Align::Left, if todo.done { FAINT } else { WHITE }, alpha);
            if todo.done {
                let line = g.text_width(&todo.text, 12.0, false).min(text_r.w);
                g.fill(Rect { x: text_r.x, y: cy, w: line, h: 1.0 }, 0.0, FAINT, alpha);
            }
            hits.push((text_r, change));
        }
        hits.push((Rect { x: r.x, y: r.y, w: 26.0, h: r.h }, toggle));
        let del = Rect { x: r.x + r.w - 24.0, y: r.y, w: 24.0, h: r.h };
        if over {
            g.text("×", del, 15.0, false, Align::Center, if is_hovered(s, &delete) { RED } else { MUTED }, alpha);
        }
        hits.push((del, delete));
    }
    if s.todos.len() > TODO_ROWS {
        let track = row_h * TODO_ROWS as f32 - 6.0;
        let y = 41.0 + track * scroll as f32 / s.todos.len() as f32;
        g.fill(Rect { x: a.x + a.w - SCROLLBAR_INSET, y, w: 2.5, h: track * TODO_ROWS as f32 / s.todos.len() as f32 }, 1.2, WHITE, 0.35 * alpha);
    }
}

fn draw_shelf(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    if s.shelf_receiving {
        let zone = Rect { x: a.x + PAD, y: 10.0, w: a.w - PAD * 2.0, h: 100.0 };
        g.fill(zone, 14.0, BLUE, 0.18 * alpha);
        g.text("Drop to add to the shelf", zone, 13.5, true, Align::Center, WHITE, alpha);
        return;
    }
    if s.shelf.is_empty() {
        draw_page_header(g, Tab::Shelf, "Shelf is empty", "Drag files onto the pill", a, alpha);
        g.text("Files stay where they are. The shelf only remembers them.", Rect { x: a.x + PAD + 4.0, y: 76.0, w: a.w - (PAD + 4.0) * 2.0, h: 28.0 }, 10.5, false, Align::Left, FAINT, alpha);
        return;
    }
    let cell = (a.w - PAD * 2.0) / SHELF_COLUMNS as f32;
    let scroll = s.page_scroll.min(s.shelf.len().saturating_sub(SHELF_COLUMNS));
    for (i, item) in s.shelf.iter().enumerate().skip(scroll).take(SHELF_COLUMNS) {
        let r = Rect { x: a.x + PAD + (i - scroll) as f32 * cell, y: 8.0, w: cell, h: 78.0 };
        let (open, remove) = (Action::ShelfOpen(item.path.clone()), Action::ShelfRemove(item.path.clone()));
        let over = is_hovered(s, &open) || is_hovered(s, &remove);
        if over {
            g.fill(r, 10.0, WHITE, 0.07 * alpha);
        }
        let tile = Rect { x: r.x + (r.w - 42.0) / 2.0, y: r.y + 7.0, w: 42.0, h: 44.0 };
        g.fill(tile, 8.0, WHITE, 0.12 * alpha);
        g.text(&item.badge, tile, 10.5, true, Align::Center, WHITE, alpha);
        g.text(&item.name, Rect { x: r.x + 4.0, y: r.y + 54.0, w: r.w - 8.0, h: 20.0 }, 10.5, false, Align::Center, MUTED, alpha);
        hits.push((r, open));
        let close = Rect { x: r.x + r.w - 20.0, y: r.y + 2.0, w: 18.0, h: 18.0 };
        if over {
            g.circle(close.x + 9.0, close.y + 9.0, 8.0, 0x2C2C2E, alpha);
            g.text("×", close, 13.0, false, Align::Center, if is_hovered(s, &remove) { RED } else { WHITE }, alpha);
        }
        hits.push((close, remove));
    }
    let count = if s.shelf.len() == 1 { "1 item".to_string() } else { format!("{} items", s.shelf.len()) };
    g.text(&count, Rect { x: a.x + PAD + 4.0, y: 92.0, w: 120.0, h: 20.0 }, 10.5, false, Align::Left, FAINT, alpha);
    draw_pill_button(g, s, "Clear", Rect { x: a.x + a.w - PAD - 52.0, y: 92.0, w: 52.0, h: 20.0 }, Action::ShelfClear, alpha, hits);
}

fn draw_calendar(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let (x, w) = (a.x + PAD + 4.0, a.w - (PAD + 4.0) * 2.0);
    let wide = Rect { x, y: 76.0, w, h: 28.0 };
    match &s.calendar.phase {
        Phase::NeedsSetup => {
            draw_page_header(g, Tab::Calendar, "Connect Google Calendar", "Add your Google client file first", a, alpha);
            let half = (w - 8.0) / 2.0;
            draw_pill_button(g, s, "Google setup", Rect { w: half, ..wide }, Action::OpenUrl(calendar::CONSOLE_URL.to_string()), alpha, hits);
            draw_pill_button(g, s, "Open folder", Rect { x: x + half + 8.0, w: half, ..wide }, Action::OpenFolder, alpha, hits);
        }
        Phase::SignedOut(why) => {
            let line = if why.is_empty() { "Read-only: TwoCode can't change events" } else { why.as_str() };
            draw_page_header(g, Tab::Calendar, "Connect Google Calendar", line, a, alpha);
            draw_pill_button(g, s, "Connect", wide, Action::CalendarConnect, alpha, hits);
        }
        Phase::Connecting => {
            draw_page_header(g, Tab::Calendar, "Waiting for your browser", "Finish signing in with Google", a, alpha);
        }
        Phase::Failed(why) => {
            draw_page_header(g, Tab::Calendar, "Couldn't load your calendar", why, a, alpha);
            draw_pill_button(g, s, "Try again", wide, Action::CalendarConnect, alpha, hits);
        }
        Phase::Ready => draw_events(g, s, a, alpha, hits),
    }
}

fn draw_events(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let upcoming: Vec<&calendar::Event> = s.calendar.events.iter().filter(|e| e.end_ms > s.now_ms).collect();
    if upcoming.is_empty() {
        draw_page_header(g, Tab::Calendar, "Nothing coming up", "Your next 7 days are clear", a, alpha);
        return;
    }
    let (x, w, row_h) = (a.x + 10.0, a.w - 20.0, 37.0);
    let scroll = s.page_scroll.min(upcoming.len().saturating_sub(EVENT_ROWS));
    let top = 7.0 + (EVENT_ROWS - upcoming.len().min(EVENT_ROWS)) as f32 * row_h / 2.0;
    for (i, event) in upcoming.iter().enumerate().skip(scroll).take(EVENT_ROWS) {
        let r = Rect { x, y: top + (i - scroll) as f32 * row_h, w, h: row_h - 2.0 };
        let open = Action::OpenUrl(event.link.clone());
        if !event.link.is_empty() {
            if is_hovered(s, &open) {
                g.fill(r, 10.0, WHITE, 0.08 * alpha);
            }
            hits.push((r, open));
        }
        let live = !event.all_day && event.start_ms <= s.now_ms;
        g.fill(Rect { x: r.x + 8.0, y: r.y + 5.0, w: 3.0, h: r.h - 10.0 }, 1.5, if live { GREEN } else { BLUE }, alpha);
        let room = r.w - 20.0 - if event.join.is_some() { 56.0 } else { 8.0 };
        g.text(&event.title, Rect { x: r.x + 18.0, y: r.y + 1.0, w: room, h: 18.0 }, 12.0, true, Align::Left, WHITE, alpha);
        g.text(&calendar::when(event, s.now_ms), Rect { x: r.x + 18.0, y: r.y + 17.0, w: room, h: 16.0 }, 10.5, false, Align::Left, if live { GREEN } else { MUTED }, alpha);
        if let Some(join) = &event.join {
            draw_pill_button(g, s, "Join", Rect { x: r.x + r.w - 52.0, y: r.y + 7.0, w: 44.0, h: 21.0 }, Action::OpenUrl(join.clone()), alpha, hits);
        }
    }
}

/// One session up close: what it needs from you, or what it is doing right now.
fn draw_session(g: &mut Gfx, s: &Scene, v: &SessionView, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let (x, w) = (a.x + 16.0, a.w - 32.0);
    let back = Rect { x: x - 6.0, y: 6.0, w: 26.0, h: 24.0 };
    if is_hovered(s, &Action::Back) {
        g.fill(back, 12.0, WHITE, 0.12 * alpha);
    }
    g.text("‹", back, 19.0, false, Align::Center, WHITE, alpha);
    hits.push((back, Action::Back));
    let (name, tint) = brand(v.agent);
    g.text(name, Rect { x: x + 24.0, y: 8.0, w: 44.0, h: 20.0 }, 11.5, true, Align::Left, tint, alpha);
    let status = if v.elapsed_s > 0 { format!("{} · {}", v.status_text(), layout::elapsed(v.elapsed_s)) } else { v.status_text() };
    let (status_w, open_w) = (150.0, 46.0);
    let jump = Action::Jump(v.agent, v.id.clone());
    let open_r = Rect { x: x + w - open_w, y: 8.0, w: open_w, h: 20.0 };
    g.fill(open_r, 10.0, WHITE, if is_hovered(s, &jump) { 0.24 } else { 0.12 } * alpha);
    g.text("Open", open_r, 10.5, true, Align::Center, WHITE, alpha);
    hits.push((open_r, jump));
    let status_x = x + w - open_w - 8.0 - status_w;
    g.text(&v.project, Rect { x: x + 68.0, y: 8.0, w: status_x - x - 68.0, h: 20.0 }, 12.5, true, Align::Left, WHITE, alpha);
    g.text(&status, Rect { x: status_x, y: 8.0, w: status_w, h: 20.0 }, 11.0, true, Align::Right, state_color(v), alpha);

    if v.can_decide {
        g.text(&v.approval.title, Rect { x, y: 32.0, w, h: 15.0 }, 10.5, false, Align::Left, FAINT, alpha);
        let box_r = Rect { x, y: 48.0, w, h: 38.0 };
        g.fill(box_r, 8.0, WHITE, 0.08 * alpha);
        let body = Rect { x: x + 8.0, y: 50.0, w: w - 16.0, h: 34.0 };
        if v.approval.diff.is_empty() {
            g.text_wrapped(&v.approval.what, body, 11.5, false, Align::Left, WHITE, alpha);
        } else {
            for (i, line) in v.approval.diff.iter().take(2).enumerate() {
                let (mark, hex) = if line.added { ("+", GREEN) } else { ("-", RED) };
                let row = Rect { y: body.y + i as f32 * 17.0, h: 17.0, ..body };
                g.text(&format!("{mark} {}", line.text), row, 11.0, false, Align::Left, hex, alpha);
            }
        }
        let (deny, allow) = (Action::Deny(v.agent, v.id.clone()), Action::Allow(v.agent, v.id.clone()));
        let half = (w - 8.0) / 2.0;
        let deny_r = Rect { x, y: 91.0, w: half, h: 25.0 };
        let allow_r = Rect { x: x + half + 8.0, y: 91.0, w: half, h: 25.0 };
        g.fill(deny_r, 12.5, WHITE, if is_hovered(s, &deny) { 0.24 } else { 0.14 } * alpha);
        g.text("Deny", deny_r, 12.0, true, Align::Center, WHITE, alpha);
        g.fill(allow_r, 12.5, GREEN, if is_hovered(s, &allow) { 1.0 } else { 0.86 } * alpha);
        g.text("Allow", allow_r, 12.0, true, Align::Center, 0x052E16, alpha);
        hits.push((deny_r, deny));
        hits.push((allow_r, allow));
        return;
    }
    if v.status == Status::Waiting {
        let question = if v.said.is_empty() { v.detail.as_str() } else { v.said.as_str() };
        g.text_wrapped(question, Rect { x, y: 34.0, w, h: 56.0 }, 13.0, false, Align::Left, WHITE, alpha);
        let hint = format!("{} · reply in {}", v.detail, brand(v.agent).0);
        g.text(&hint, Rect { x, y: 94.0, w, h: 18.0 }, 11.0, true, Align::Left, AMBER, alpha);
        return;
    }
    if !v.prompt.is_empty() {
        g.text(&v.prompt, Rect { x, y: 29.0, w, h: 16.0 }, 11.0, false, Align::Left, MUTED, alpha);
    }
    if v.status == Status::Done {
        g.text(&v.summary(), Rect { x, y: 47.0, w, h: 18.0 }, 12.5, true, Align::Left, WHITE, alpha);
        let body = if v.files.is_empty() { v.said.clone() } else { v.files.join(", ") };
        g.text_wrapped(&body, Rect { x, y: 66.0, w, h: 50.0 }, 11.0, false, Align::Left, MUTED, alpha);
        return;
    }
    if v.status == Status::Error {
        g.text(&v.detail, Rect { x, y: 47.0, w, h: 18.0 }, 12.5, true, Align::Left, RED, alpha);
        g.text_wrapped(&v.said, Rect { x, y: 66.0, w, h: 50.0 }, 11.0, false, Align::Left, MUTED, alpha);
        return;
    }
    if !v.plan.is_empty() {
        draw_checklist(g, v, Rect { x, y: 47.0, w, h: 50.0 }, alpha);
        let hint = stuck_hint(v);
        let live = hint.as_deref().unwrap_or(v.detail.as_str());
        g.text(live, Rect { x, y: 98.0, w, h: 18.0 }, 10.5, false, Align::Left, if v.is_quiet() { AMBER } else { FAINT }, alpha);
        return;
    }
    let said = if v.said.is_empty() { &v.detail } else { &v.said };
    g.text_wrapped(said, Rect { x, y: 46.0, w, h: 34.0 }, 11.5, false, Align::Left, WHITE, alpha);
    let mut y = 81.0;
    if let Some(hint) = stuck_hint(v) {
        g.text(&hint, Rect { x, y, w, h: 16.0 }, 10.5, false, Align::Left, AMBER, alpha);
        y += 16.0;
    }
    let rows = ((116.0 - y) / 16.0) as usize;
    for step in v.steps.iter().rev().take(rows) {
        let (mark, text_hex, opacity) = if step.done { (FAINT, MUTED, 0.75) } else { (BLUE, WHITE, 1.0) };
        g.circle(x + 3.0, y + 8.0, 2.5, mark, alpha);
        g.text(&step.label, Rect { x: x + 12.0, y, w: w - 12.0, h: 16.0 }, 11.0, !step.done, Align::Left, text_hex, opacity * alpha);
        y += 16.0;
    }
}

fn stuck_hint(v: &SessionView) -> Option<String> {
    if !v.is_quiet() {
        return None;
    }
    Some(match &v.repeated_error {
        Some((error, _)) => error.clone(),
        None => "It may be on a long command or waiting in its own window.".to_string(),
    })
}

/// What finished in the last few hours, for when no agent is working.
fn draw_runs(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let shown = s.runs.len().min(RUN_ROWS);
    let scroll = s.runs_scroll.min(s.runs.len() - shown);
    let row_h = 37.0;
    let (x, w, top) = (a.x + 10.0, a.w - 20.0, 7.0 + (RUN_ROWS - shown) as f32 * row_h / 2.0);
    for (i, run) in s.runs.iter().enumerate().skip(scroll).take(RUN_ROWS) {
        let r = Rect { x, y: top + (i - scroll) as f32 * row_h, w, h: row_h - 2.0 };
        let action = Action::OpenRun(run.at_ms);
        if is_hovered(s, &action) {
            g.fill(r, 10.0, WHITE, 0.08 * alpha);
        }
        let (name, tint) = brand(run.agent);
        g.circle(r.x + 11.0, r.y + 11.0, 4.0, if run.failed { RED } else { GREEN }, alpha);
        g.text(name, Rect { x: r.x + 22.0, y: r.y + 1.0, w: 44.0, h: 19.0 }, 11.5, true, Align::Left, tint, alpha);
        g.text(&run.project, Rect { x: r.x + 66.0, y: r.y + 1.0, w: r.w - 66.0 - 78.0, h: 19.0 }, 12.0, true, Align::Left, WHITE, alpha);
        let when = layout::ago(s.now_ms.saturating_sub(run.at_ms));
        g.text(&when, Rect { x: r.x + r.w - 74.0, y: r.y + 1.0, w: 66.0, h: 19.0 }, 10.5, false, Align::Right, FAINT, alpha);
        g.text(&run.summary, Rect { x: r.x + 22.0, y: r.y + 17.0, w: r.w - 30.0, h: 17.0 }, 11.0, false, Align::Left, MUTED, alpha);
        hits.push((r, action));
    }
    if s.runs.len() > RUN_ROWS {
        let track = row_h * RUN_ROWS as f32 - 6.0;
        let thumb = track * RUN_ROWS as f32 / s.runs.len() as f32;
        let y = top + 2.0 + track * scroll as f32 / s.runs.len() as f32;
        g.fill(Rect { x: a.x + a.w - SCROLLBAR_INSET, y, w: 2.5, h: thumb }, 1.2, WHITE, 0.35 * alpha);
    }
}

fn draw_run(g: &mut Gfx, s: &Scene, run: &Run, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let (x, w) = (a.x + 16.0, a.w - 32.0);
    let back = Rect { x: x - 6.0, y: 6.0, w: 26.0, h: 24.0 };
    if is_hovered(s, &Action::Back) {
        g.fill(back, 12.0, WHITE, 0.12 * alpha);
    }
    g.text("‹", back, 19.0, false, Align::Center, WHITE, alpha);
    hits.push((back, Action::Back));
    let (name, tint) = brand(run.agent);
    g.text(name, Rect { x: x + 24.0, y: 8.0, w: 44.0, h: 20.0 }, 11.5, true, Align::Left, tint, alpha);
    let (when_w, open_w) = (70.0, 46.0);
    let jump = Action::JumpRun(run.at_ms);
    let open_r = Rect { x: x + w - open_w, y: 8.0, w: open_w, h: 20.0 };
    g.fill(open_r, 10.0, WHITE, if is_hovered(s, &jump) { 0.24 } else { 0.12 } * alpha);
    g.text("Open", open_r, 10.5, true, Align::Center, WHITE, alpha);
    hits.push((open_r, jump));
    let when_x = x + w - open_w - 8.0 - when_w;
    g.text(&run.project, Rect { x: x + 68.0, y: 8.0, w: when_x - x - 68.0, h: 20.0 }, 12.5, true, Align::Left, WHITE, alpha);
    g.text(&layout::ago(s.now_ms.saturating_sub(run.at_ms)), Rect { x: when_x, y: 8.0, w: when_w, h: 20.0 }, 10.5, false, Align::Right, FAINT, alpha);
    g.text(&run.prompt, Rect { x, y: 29.0, w, h: 16.0 }, 11.0, false, Align::Left, MUTED, alpha);
    g.text(&run.summary, Rect { x, y: 47.0, w, h: 18.0 }, 12.5, true, Align::Left, if run.failed { RED } else { WHITE }, alpha);
    g.text_wrapped(&run.files.join(", "), Rect { x, y: 66.0, w, h: 50.0 }, 11.0, false, Align::Left, MUTED, alpha);
}

/// Three lines of the task list, kept around the step in progress.
fn draw_checklist(g: &mut Gfx, v: &SessionView, r: Rect, alpha: f32) {
    const LINES: usize = 3;
    let at = v.plan.iter().position(|i| i.state == PlanState::Active).unwrap_or(0);
    let first = at.saturating_sub(1).min(v.plan.len().saturating_sub(LINES));
    for (row, (n, item)) in v.plan.iter().enumerate().skip(first).take(LINES).enumerate() {
        let y = r.y + row as f32 * 16.5;
        let (mark, hex, bold, opacity) = match item.state {
            PlanState::Done => (GREEN, MUTED, false, 0.8),
            PlanState::Active => (BLUE, WHITE, true, 1.0),
            PlanState::Pending => (FAINT, MUTED, false, 0.8),
        };
        g.circle(r.x + 3.0, y + 8.0, 2.5, mark, alpha);
        let label = format!("{}. {}", n + 1, item.text);
        g.text(&label, Rect { x: r.x + 12.0, y, w: r.w - 12.0, h: 16.0 }, 11.0, bold, Align::Left, hex, opacity * alpha);
    }
}
