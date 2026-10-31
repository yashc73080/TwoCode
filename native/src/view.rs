//! What the notch shows in each state, and which parts of it you can click.
use crate::agents::{Agent, SessionView, Status};
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
const MUSIC_W: f32 = 270.0;

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
    g.fill(body, s.shape.radius, 0x000000, 1.0);
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

/// Small dots along the bottom, the chosen one stretched. The Agents dot takes the colour of their state.
fn draw_dots(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let (dot, wide, gap, cy) = (4.0, 14.0, 6.0, s.open_h - 8.0);
    let total = wide + (s.tabs.len() - 1) as f32 * dot + (s.tabs.len() - 1) as f32 * gap;
    let mut x = a.x + (a.w - total) / 2.0;
    for tab in s.tabs {
        let chosen = *tab == s.tab;
        let w = if chosen { wide } else { dot };
        let action = Action::Tab(*tab);
        let status = if *tab == Tab::Agents && !chosen { layout::light(s.sessions.iter().map(|v| v.status)) } else { None };
        let (hex, opacity) = match status {
            Some(status) => (status_colour(status), 1.0),
            None if chosen => (WHITE, 0.95),
            None if is_hovered(s, &action) => (WHITE, 0.8),
            None => (WHITE, 0.35),
        };
        g.fill(Rect { x, y: cy - dot / 2.0, w, h: dot }, dot / 2.0, hex, opacity * alpha);
        hits.push((Rect { x: x - gap / 2.0, y: cy - 8.0, w: w + gap, h: 16.0 }, action));
        x += w + gap;
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

    if split {
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

/// Sessions as cards side by side, which suits a wide and shallow notch.
fn draw_agents(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    if s.sessions.is_empty() {
        g.text("No active agents", Rect { h: a.h - 14.0, ..a }, 13.0, false, Align::Center, FAINT, alpha);
        return;
    }
    let shown = s.sessions.len().min(3);
    let (gap, top, height) = (8.0, 10.0, s.open_h - 10.0 - 18.0);
    let width = (a.w - PAD * 2.0 - gap * (shown - 1) as f32) / shown as f32;
    for (i, v) in s.sessions.iter().take(shown).enumerate() {
        let card = Rect { x: a.x + PAD + i as f32 * (width + gap), y: top, w: width, h: height };
        draw_card(g, s, v, card, alpha, hits);
    }
    if s.sessions.len() > 3 {
        let more = format!("+{} more", s.sessions.len() - 3);
        g.text(&more, Rect { x: a.x + a.w - PAD - 80.0, y: s.open_h - 16.0, w: 80.0, h: 14.0 }, 10.0, false, Align::Right, FAINT, alpha);
    }
}

fn draw_card(g: &mut Gfx, s: &Scene, v: &SessionView, r: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    g.fill(r, 14.0, WHITE, 0.075 * alpha);
    let (x, w) = (r.x + 12.0, r.w - 24.0);
    let (brand, tint) = match v.agent {
        Agent::Claude => ("Claude", CLAUDE),
        Agent::Codex => ("Codex", CODEX),
    };
    g.circle(x + 4.0, r.y + 19.0, 4.0, status_colour(v.status), alpha);
    g.text(brand, Rect { x: x + 14.0, y: r.y + 9.0, w: 60.0, h: 20.0 }, 11.5, true, Align::Left, tint, alpha);
    if v.elapsed_s > 0 && !v.can_decide {
        g.text(&layout::elapsed(v.elapsed_s), Rect { x: x + w - 60.0, y: r.y + 9.0, w: 60.0, h: 20.0 }, 10.5, false, Align::Right, FAINT, alpha);
    }
    g.text(&v.project, Rect { x, y: r.y + 30.0, w, h: 20.0 }, 13.0, true, Align::Left, WHITE, alpha);
    g.text(&v.detail, Rect { x, y: r.y + 50.0, w, h: 18.0 }, 11.0, false, Align::Left, MUTED, alpha);
    if v.can_decide {
        let y = r.y + r.h - 30.0;
        let half = (w - 6.0) / 2.0;
        let deny = Action::Deny(v.agent, v.id.clone());
        let allow = Action::Allow(v.agent, v.id.clone());
        let deny_r = Rect { x, y, w: half, h: 24.0 };
        let allow_r = Rect { x: x + half + 6.0, y, w: half, h: 24.0 };
        g.fill(deny_r, 12.0, WHITE, if is_hovered(s, &deny) { 0.24 } else { 0.14 } * alpha);
        g.text("Deny", deny_r, 11.5, true, Align::Center, WHITE, alpha);
        g.fill(allow_r, 12.0, GREEN, if is_hovered(s, &allow) { 1.0 } else { 0.86 } * alpha);
        g.text("Allow", allow_r, 11.5, true, Align::Center, 0x052E16, alpha);
        hits.push((deny_r, deny));
        hits.push((allow_r, allow));
    }
}
