//! What the notch shows in each state, and which parts of it you can click.
use crate::agents::{Agent, SessionView, Status};
use crate::gfx::{Align, Gfx, Rect};
use crate::layout::{self, Mode, Shape, CANVAS_W};
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

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    Previous,
    Toggle,
    Next,
    Seek { x: f32, w: f32, duration_ms: u64 },
    Allow(Agent, String),
    Deny(Agent, String),
}

pub struct Scene<'a> {
    pub shape: Shape,
    pub mode: Mode,
    pub open_h: f32,
    pub show_media: bool,
    pub show_agents: bool,
    pub minimal: bool,
    pub media: &'a Media,
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
    let area = Rect { x: left, y: 0.0, w: s.shape.w, h: s.shape.h };
    // The top corners sit above the screen edge, so the notch grows out of the bezel.
    g.fill(Rect { x: left, y: -s.shape.radius, w: s.shape.w, h: s.shape.h + s.shape.radius }, s.shape.radius, 0x000000, 1.0);
    g.clip(area);
    let open = layout::content_alpha(s.shape.h, s.open_h);
    match s.mode {
        Mode::Open if open > 0.0 => draw_open(g, s, area, open, hits),
        Mode::Open => {}
        Mode::Peek if s.shape.h > 30.0 => draw_peek(g, s, area),
        Mode::Peek => {}
        Mode::Rest if s.minimal => draw_line(g, s, area),
        Mode::Rest if s.shape.h < 40.0 => draw_rest(g, s, area),
        Mode::Rest => {}
    }
    g.unclip();
}

fn light(s: &Scene) -> Option<Status> {
    if !s.show_agents {
        return None;
    }
    layout::light(s.sessions.iter().map(|v| v.status))
}

fn draw_line(g: &mut Gfx, s: &Scene, a: Rect) {
    if let Some(status) = light(s) {
        g.fill(Rect { x: a.x + a.w / 2.0 - 18.0, y: a.h - 2.5, w: 36.0, h: 2.0 }, 1.0, status_colour(status), 1.0);
    }
}

fn draw_rest(g: &mut Gfx, s: &Scene, a: Rect) {
    let cy = a.h / 2.0;
    if let Some(status) = light(s) {
        g.circle(a.x + 20.0, cy, 4.0, status_colour(status), 1.0);
        let count = s.sessions.iter().filter(|v| v.status == Status::Running || v.status == Status::Waiting).count();
        if count > 1 {
            g.text(&count.to_string(), Rect { x: a.x + 28.0, y: 0.0, w: 22.0, h: a.h }, 11.0, true, Align::Left, MUTED, 1.0);
        }
    }
    if s.show_media && s.media.playing {
        bars(g, a.x + a.w - 34.0, cy);
    }
}

/// A still level meter. It does not dance, because a moving picture would keep the graphics card awake.
fn bars(g: &mut Gfx, x: f32, cy: f32) {
    for (i, h) in [6.0, 11.0, 8.0, 12.0].iter().enumerate() {
        g.fill(Rect { x: x + i as f32 * 4.5, y: cy - h / 2.0, w: 2.5, h: *h }, 1.2, WHITE, 0.85);
    }
}

fn draw_peek(g: &mut Gfx, s: &Scene, a: Rect) {
    let Some(text) = s.peek else { return };
    g.circle(a.x + 24.0, a.h / 2.0, 5.0, status_colour(s.peek_status), 1.0);
    g.text(text, Rect { x: a.x + 38.0, y: 0.0, w: a.w - 54.0, h: a.h }, 13.0, true, Align::Left, WHITE, 1.0);
}

fn draw_open(g: &mut Gfx, s: &Scene, a: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let pad = 20.0;
    let top = 14.0;
    let body = Rect { x: a.x + pad, y: top, w: a.w - pad * 2.0, h: s.open_h - top - 14.0 };
    match (s.show_media, s.show_agents) {
        (true, true) => {
            let music_w = 290.0;
            draw_music(g, s, Rect { w: music_w, ..body }, alpha, hits);
            g.fill(Rect { x: body.x + music_w + 14.0, y: body.y + 4.0, w: 1.0, h: body.h - 8.0 }, 0.0, WHITE, 0.09 * alpha);
            draw_agents(g, s, Rect { x: body.x + music_w + 29.0, w: body.w - music_w - 29.0, ..body }, alpha, hits);
        }
        (true, false) => draw_music(g, s, body, alpha, hits),
        (false, true) => draw_agents(g, s, body, alpha, hits),
        _ => {}
    }
}

fn is_hovered(s: &Scene, action: &Action) -> bool {
    match (s.hovered, action) {
        (Some(Action::Seek { .. }), Action::Seek { .. }) => true,
        (Some(a), b) => a == b,
        _ => false,
    }
}

fn draw_music(g: &mut Gfx, s: &Scene, r: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let m = s.media;
    if !m.present {
        g.text("Nothing playing", r, 13.0, false, Align::Center, FAINT, alpha);
        return;
    }
    let art = Rect { x: r.x, y: r.y + 2.0, w: 78.0, h: 78.0 };
    if g.has_art() {
        g.draw_art(art, 12.0, alpha);
    } else {
        g.fill(art, 12.0, WHITE, 0.08 * alpha);
        g.polygon(&[(art.x + 31.0, art.y + 26.0), (art.x + 53.0, art.y + 39.0), (art.x + 31.0, art.y + 52.0)], WHITE, 0.45 * alpha);
    }
    let tx = art.x + art.w + 12.0;
    let tw = r.x + r.w - tx;
    let title = if m.title.is_empty() { "Unknown" } else { &m.title };
    g.text(title, Rect { x: tx, y: r.y + 4.0, w: tw, h: 22.0 }, 14.0, true, Align::Left, WHITE, alpha);
    g.text(&m.artist, Rect { x: tx, y: r.y + 26.0, w: tw, h: 19.0 }, 12.0, false, Align::Left, MUTED, alpha);
    g.text(&m.app, Rect { x: tx, y: r.y + 45.0, w: tw, h: 17.0 }, 10.5, false, Align::Left, FAINT, alpha);

    // progress
    let bar_y = r.y + 99.0;
    let now = m.position_now();
    if m.duration_ms > 0 {
        let seek = Action::Seek { x: r.x, w: r.w, duration_ms: m.duration_ms };
        let grow = if m.can_seek && is_hovered(s, &seek) { 1.5 } else { 0.0 };
        let fraction = (now as f32 / m.duration_ms as f32).clamp(0.0, 1.0);
        g.fill(Rect { x: r.x, y: bar_y - grow / 2.0, w: r.w, h: 3.0 + grow }, 2.0, WHITE, 0.18 * alpha);
        g.fill(Rect { x: r.x, y: bar_y - grow / 2.0, w: (r.w * fraction).max(3.0), h: 3.0 + grow }, 2.0, WHITE, 0.92 * alpha);
        if m.can_seek {
            hits.push((Rect { x: r.x, y: bar_y - 7.0, w: r.w, h: 17.0 }, seek));
        }
        g.text(&layout::clock(now), Rect { x: r.x, y: bar_y + 6.0, w: 60.0, h: 16.0 }, 10.5, false, Align::Left, FAINT, alpha);
        g.text(&layout::clock(m.duration_ms), Rect { x: r.x + r.w - 60.0, y: bar_y + 6.0, w: 60.0, h: 16.0 }, 10.5, false, Align::Right, FAINT, alpha);
    }

    // controls
    let cx = r.x + r.w / 2.0;
    let cy = r.y + r.h - 21.0;
    for (offset, action) in [(-54.0, Action::Previous), (0.0, Action::Toggle), (54.0, Action::Next)] {
        let box_r = Rect { x: cx + offset - 19.0, y: cy - 19.0, w: 38.0, h: 38.0 };
        if is_hovered(s, &action) {
            g.fill(box_r, 19.0, WHITE, 0.12 * alpha);
        }
        let x = cx + offset;
        match action {
            Action::Previous => {
                g.fill(Rect { x: x - 9.0, y: cy - 7.0, w: 2.5, h: 14.0 }, 1.0, WHITE, alpha);
                g.polygon(&[(x + 8.0, cy - 7.0), (x + 8.0, cy + 7.0), (x - 5.0, cy)], WHITE, alpha);
            }
            Action::Next => {
                g.fill(Rect { x: x + 6.5, y: cy - 7.0, w: 2.5, h: 14.0 }, 1.0, WHITE, alpha);
                g.polygon(&[(x - 8.0, cy - 7.0), (x - 8.0, cy + 7.0), (x + 5.0, cy)], WHITE, alpha);
            }
            _ if m.playing => {
                g.fill(Rect { x: x - 6.5, y: cy - 8.0, w: 4.5, h: 16.0 }, 1.2, WHITE, alpha);
                g.fill(Rect { x: x + 2.0, y: cy - 8.0, w: 4.5, h: 16.0 }, 1.2, WHITE, alpha);
            }
            _ => g.polygon(&[(x - 6.0, cy - 9.0), (x - 6.0, cy + 9.0), (x + 9.0, cy)], WHITE, alpha),
        }
        hits.push((box_r, action));
    }
}

fn draw_agents(g: &mut Gfx, s: &Scene, r: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    if s.sessions.is_empty() {
        g.text("No active agents", r, 13.0, false, Align::Center, FAINT, alpha);
        return;
    }
    let row_h = 50.0;
    let shown = s.sessions.iter().take(3);
    for (i, v) in shown.enumerate() {
        draw_row(g, s, v, Rect { x: r.x, y: r.y + i as f32 * row_h, w: r.w, h: row_h - 4.0 }, alpha, hits);
    }
    if s.sessions.len() > 3 {
        let more = format!("+{} more", s.sessions.len() - 3);
        g.text(&more, Rect { x: r.x, y: r.y + r.h - 12.0, w: r.w, h: 14.0 }, 10.0, false, Align::Right, FAINT, alpha);
    }
}

fn draw_row(g: &mut Gfx, s: &Scene, v: &SessionView, r: Rect, alpha: f32, hits: &mut Vec<(Rect, Action)>) {
    let cy = r.y + r.h / 2.0;
    g.circle(r.x + 5.0, cy, 4.5, status_colour(v.status), alpha);
    let buttons = if v.can_decide { 124.0 } else { 0.0 };
    let tx = r.x + 19.0;
    let tw = r.w - 19.0 - buttons;
    let (brand, tint) = match v.agent {
        Agent::Claude => ("Claude", CLAUDE),
        Agent::Codex => ("Codex", CODEX),
    };
    g.text(brand, Rect { x: tx, y: r.y + 3.0, w: 46.0, h: 20.0 }, 12.0, true, Align::Left, tint, alpha);
    let time = if v.can_decide { 0.0 } else { 58.0 };
    g.text(&v.project, Rect { x: tx + 46.0, y: r.y + 3.0, w: tw - 46.0 - time, h: 20.0 }, 12.5, true, Align::Left, WHITE, alpha);
    if !v.can_decide && v.elapsed_s > 0 {
        g.text(&layout::elapsed(v.elapsed_s), Rect { x: r.x + r.w - time, y: r.y + 3.0, w: time, h: 20.0 }, 10.5, false, Align::Right, FAINT, alpha);
    }
    g.text(&v.detail, Rect { x: tx, y: r.y + 23.0, w: tw, h: 18.0 }, 11.5, false, Align::Left, MUTED, alpha);
    if v.can_decide {
        let y = cy - 14.0;
        let deny = Action::Deny(v.agent, v.id.clone());
        let allow = Action::Allow(v.agent, v.id.clone());
        let deny_r = Rect { x: r.x + r.w - 118.0, y, w: 54.0, h: 28.0 };
        let allow_r = Rect { x: r.x + r.w - 58.0, y, w: 58.0, h: 28.0 };
        g.fill(deny_r, 14.0, WHITE, if is_hovered(s, &deny) { 0.22 } else { 0.12 } * alpha);
        g.text("Deny", deny_r, 12.0, true, Align::Center, WHITE, alpha);
        g.fill(allow_r, 14.0, GREEN, if is_hovered(s, &allow) { 1.0 } else { 0.86 } * alpha);
        g.text("Allow", allow_r, 12.0, true, Align::Center, 0x052E16, alpha);
        hits.push((deny_r, deny));
        hits.push((allow_r, allow));
    }
}
