//! What the notch shows in each state, and which parts of it you can click.
use crate::agents::{Agent, Pill, PlanState, SessionView, Status};
use crate::gfx::{Align, Gfx, Rect};
use crate::layout::{self, Mode, Shape, Tab, CANVAS_W};
use crate::lyrics::{self, Line};
use crate::media::Media;

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
const MUSIC_W: f32 = 250.0;

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    Previous,
    Toggle,
    Next,
    ToggleLyrics,
    Seek { x: f32, w: f32, duration_ms: u64 },
    SeekTo(u64),
    Allow(Agent, String),
    Deny(Agent, String),
    Tab(Tab),
    Open(Agent, String),
    Back,
}

pub struct Scene<'a> {
    pub shape: Shape,
    pub mode: Mode,
    pub open_h: f32,
    pub tabs: &'a [Tab],
    pub tab: Tab,
    pub media: &'a Media,
    pub lyrics_on: bool,
    pub lyric_lines: &'a [Line],
    pub sessions: &'a [SessionView],
    pub selected: Option<&'a SessionView>,
    pub pill: Pill,
    pub peek: Option<&'a str>,
    pub peek_status: Status,
    pub hovered: Option<&'a Action>,
}

fn status_colour(status: Status) -> u32 {
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
        Mode::Peek if s.shape.h > 22.0 => draw_peek(g, s, area),
        _ => {}
    }
    g.unclip();
}

fn draw_peek(g: &mut Gfx, s: &Scene, a: Rect) {
    let Some(text) = s.peek else { return };
    g.circle(a.x + 20.0, a.h / 2.0, 4.0, status_colour(s.peek_status), 1.0);
    g.text(text, Rect { x: a.x + 32.0, y: 0.0, w: a.w - 46.0, h: a.h }, 12.0, true, Align::Left, WHITE, 1.0);
}

fn draw_open(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    match s.tab {
        Tab::Media => draw_music(g, s, a, alpha, hits),
        Tab::Agents => draw_agents(g, s, a, alpha, hits),
    }
    if s.tabs.len() > 1 {
        draw_dots(g, s, a, alpha, hits);
    }
}

/// Dots along the bottom, the chosen one stretched. Pointing at them swaps them for icons.
fn draw_dots(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    if matches!(s.hovered, Some(Action::Tab(_))) {
        draw_tab_icons(g, s, a, alpha, hits);
        return;
    }
    let (dot, wide, gap, cy) = (6.0, 20.0, 7.0, s.open_h - 9.0);
    let count = s.tabs.len() as f32;
    let total = wide + (count - 1.0) * (dot + gap);
    let mut x = a.x + (a.w - total) / 2.0;
    for tab in s.tabs {
        let chosen = *tab == s.tab;
        let w = if chosen { wide } else { dot };
        let status = if *tab == Tab::Agents && !chosen { layout::light(s.sessions.iter().map(|v| v.status)) } else { None };
        let (hex, opacity) = match status {
            Some(status) => (status_colour(status), 1.0),
            None if chosen => (WHITE, 0.95),
            None => (WHITE, 0.4),
        };
        g.fill(Rect { x, y: cy - dot / 2.0, w, h: dot }, dot / 2.0, hex, opacity * alpha);
        hits.push((Rect { x: x - gap / 2.0, y: cy - 11.0, w: w + gap, h: 20.0 }, Action::Tab(*tab)));
        x += w + gap;
    }
}

fn draw_tab_icons(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    // Low and slim, in the strip below the page, so it never covers the controls.
    let (cell, h) = (30.0, 22.0);
    let total = s.tabs.len() as f32 * cell;
    let top = s.open_h - h - 1.0;
    let tray = Rect { x: a.x + (a.w - total) / 2.0 - 4.0, y: top, w: total + 8.0, h };
    g.fill(tray, h / 2.0, 0x1C1C1E, alpha);
    for (i, tab) in s.tabs.iter().enumerate() {
        let r = Rect { x: tray.x + 4.0 + i as f32 * cell, y: top, w: cell, h };
        let action = Action::Tab(*tab);
        let fill = if *tab == s.tab { 0.2 } else if is_hovered(s, &action) { 0.1 } else { 0.0 };
        if fill > 0.0 {
            g.fill(Rect { x: r.x + 2.0, y: r.y + 2.0, w: r.w - 4.0, h: r.h - 4.0 }, (r.h - 4.0) / 2.0, WHITE, fill * alpha);
        }
        draw_tab_icon(g, *tab, r.x + r.w / 2.0, r.y + r.h / 2.0 + 1.0, alpha);
        hits.push((Rect { y: top, h: s.open_h - top, ..r }, action));
    }
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
        g.text("Nothing playing", Rect { h: a.h - 14.0, ..a }, 13.0, false, Align::Center, FAINT, alpha);
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
    let tw = x + w - tx;
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
    let (bx, by) = (bubble.x + 14.0, cy);
    g.fill(Rect { x: bx - 8.0, y: by - 6.5, w: 16.0, h: 11.0 }, 3.5, WHITE, shade * alpha);
    g.polygon(&[(bx - 4.0, by + 3.5), (bx + 1.0, by + 3.5), (bx - 4.0, by + 8.0)], WHITE, shade * alpha);
    hits.push((bubble, Action::ToggleLyrics));
    for (offset, action) in [(-40.0, Action::Previous), (0.0, Action::Toggle), (40.0, Action::Next)] {
        let box_r = Rect { x: cx + offset - 14.0, y: cy - 14.0, w: 28.0, h: 28.0 };
        if is_hovered(s, &action) {
            g.fill(box_r, 14.0, WHITE, 0.12 * alpha);
        }
        let px = cx + offset;
        match action {
            Action::Previous => {
                g.polygon(&[(px, cy - 6.0), (px, cy + 6.0), (px - 8.0, cy)], WHITE, alpha);
                g.polygon(&[(px + 8.0, cy - 6.0), (px + 8.0, cy + 6.0), (px, cy)], WHITE, alpha);
            }
            Action::Next => {
                g.polygon(&[(px, cy - 6.0), (px, cy + 6.0), (px + 8.0, cy)], WHITE, alpha);
                g.polygon(&[(px - 8.0, cy - 6.0), (px - 8.0, cy + 6.0), (px, cy)], WHITE, alpha);
            }
            _ if m.playing => {
                g.fill(Rect { x: px - 5.0, y: cy - 7.0, w: 3.5, h: 14.0 }, 1.0, WHITE, alpha);
                g.fill(Rect { x: px + 1.5, y: cy - 7.0, w: 3.5, h: 14.0 }, 1.0, WHITE, alpha);
            }
            _ => g.polygon(&[(px - 5.0, cy - 8.0), (px - 5.0, cy + 8.0), (px + 8.0, cy)], WHITE, alpha),
        }
        hits.push((box_r, action));
    }

    if split && a.w > MUSIC_W + 120.0 {
        let lx = x + w + 16.0;
        g.fill(Rect { x: lx - 8.0, y: 14.0, w: 1.0, h: 100.0 }, 0.0, WHITE, 0.1 * alpha);
        draw_lyrics(g, s, Rect { x: lx, y: 8.0, w: a.x + a.w - lx - PAD, h: 112.0 }, now, alpha, hits);
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

fn state_colour(v: &SessionView) -> u32 {
    if v.is_quiet() {
        AMBER
    } else {
        status_colour(v.status)
    }
}

fn draw_agents(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    if let Some(v) = s.selected {
        draw_session(g, s, v, a, alpha, hits);
    } else if s.sessions.is_empty() {
        g.text("No active agents", Rect { h: a.h - 16.0, ..a }, 13.0, false, Align::Center, FAINT, alpha);
    } else {
        draw_list(g, s, a, alpha, hits);
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
    g.circle(x + 4.0, 17.0, 4.0, state_colour(v), alpha);
    g.text(name, Rect { x: x + 14.0, y: 8.0, w: 44.0, h: 18.0 }, 11.5, true, Align::Left, tint, alpha);
    let status = if v.elapsed_s > 0 { format!("{} · {}", v.status_text(), layout::elapsed(v.elapsed_s)) } else { v.status_text() };
    g.text(&v.project, Rect { x: x + 58.0, y: 8.0, w: w - 58.0 - 136.0, h: 18.0 }, 12.0, true, Align::Left, WHITE, alpha);
    g.text(&status, Rect { x: x + w - 136.0, y: 8.0, w: 136.0, h: 18.0 }, 11.0, true, Align::Right, state_colour(v), alpha);
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
    g.circle(r.x + 14.0, r.y + 9.0, 3.0, state_colour(v), alpha);
    g.text(name, Rect { x: r.x + 24.0, y: r.y, w: 42.0, h: r.h }, 10.5, true, Align::Left, tint, alpha);
    g.text(&v.project, Rect { x: r.x + 66.0, y: r.y, w: r.w - 66.0 - 122.0, h: r.h }, 11.0, false, Align::Left, WHITE, alpha);
    g.text(&v.status_text(), Rect { x: r.x + r.w - 118.0, y: r.y, w: 108.0, h: r.h }, 10.5, true, Align::Right, state_colour(v), alpha);
    hits.push((r, action));
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
    let status_w = 170.0;
    g.text(&v.project, Rect { x: x + 68.0, y: 8.0, w: w - 68.0 - status_w, h: 20.0 }, 12.5, true, Align::Left, WHITE, alpha);
    g.text(&status, Rect { x: x + w - status_w, y: 8.0, w: status_w, h: 20.0 }, 11.0, true, Align::Right, state_colour(v), alpha);

    if v.can_decide {
        g.text("Wants to run", Rect { x, y: 32.0, w, h: 15.0 }, 10.5, false, Align::Left, FAINT, alpha);
        let box_r = Rect { x, y: 48.0, w, h: 38.0 };
        g.fill(box_r, 8.0, WHITE, 0.08 * alpha);
        let body = Rect { x: x + 8.0, y: 50.0, w: w - 16.0, h: 34.0 };
        g.text_wrapped(&v.approval, body, 11.5, false, Align::Left, WHITE, alpha);
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
        let live = if v.is_quiet() { "No news. It may be on a long command or waiting in its own window." } else { v.detail.as_str() };
        g.text(live, Rect { x, y: 98.0, w, h: 18.0 }, 10.5, false, Align::Left, if v.is_quiet() { AMBER } else { FAINT }, alpha);
        return;
    }
    let said = if v.said.is_empty() { &v.detail } else { &v.said };
    g.text_wrapped(said, Rect { x, y: 46.0, w, h: 34.0 }, 11.5, false, Align::Left, WHITE, alpha);
    let mut y = 81.0;
    if v.is_quiet() {
        g.text("No news. It may be on a long command or waiting in its own window.", Rect { x, y, w, h: 16.0 }, 10.5, false, Align::Left, AMBER, alpha);
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
