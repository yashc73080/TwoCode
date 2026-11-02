//! The notch's sizes and motion. Measured in device-independent pixels; no Windows calls, so it can be tested.
use crate::agents::Status;
use serde::{Deserialize, Serialize};

pub const CANVAS_W: f32 = 640.0;
pub const CANVAS_H: f32 = 170.0;
const DURATION_MS: f32 = 240.0;
const MEDIA_W: f32 = 320.0;
const MEDIA_LYRICS_W: f32 = 540.0;
const AGENTS_W: f32 = 440.0;
const CALENDAR_W: f32 = 380.0;
pub const LYRIC_TEXT: f32 = 12.5;
const LYRIC_PAD: f32 = 26.0;
const LYRIC_MIN_W: f32 = 140.0;
const LYRIC_MAX_W: f32 = 560.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Rest,
    Peek,
    /// Closed from the lyrics view: just the line being sung.
    Lyric,
    Open,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shape {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    /// Gap above the shape. The pill floats just below the screen edge; open shapes grow out of it.
    pub y: f32,
}

/// One page of the open notch. A new feature adds a value here and its place in `tabs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Tab {
    #[default]
    Media,
    // Settings saved before Recent joined the agents page still load.
    #[serde(alias = "Recent")]
    Agents,
    Calendar,
    Todos,
    Shelf,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Media => "Media",
            Tab::Agents => "Agents",
            Tab::Calendar => "Calendar",
            Tab::Todos => "Todos",
            Tab::Shelf => "Shelf",
        }
    }
}

pub fn tabs(media: bool, agents: bool, calendar: bool, todos: bool, shelf: bool) -> Vec<Tab> {
    [(media, Tab::Media), (agents, Tab::Agents), (calendar, Tab::Calendar), (todos, Tab::Todos), (shelf, Tab::Shelf)].into_iter().filter(|(on, _)| *on).map(|(_, tab)| tab).collect()
}

pub fn shape_for(mode: Mode) -> Shape {
    match mode {
        Mode::Rest => Shape { w: 56.0, h: 4.0, radius: 2.0, y: 2.0 },
        Mode::Peek => Shape { w: 280.0, h: 30.0, radius: 13.0, y: 0.0 },
        Mode::Lyric => lyric_shape(0.0),
        Mode::Open => open_shape(MEDIA_W),
    }
}

/// Just wide enough for the line, within limits, and no deeper than it has to be.
pub fn lyric_shape(text_w: f32) -> Shape {
    Shape { w: (text_w + LYRIC_PAD * 2.0).clamp(LYRIC_MIN_W, LYRIC_MAX_W), h: 26.0, radius: 12.0, y: 0.0 }
}

pub fn open_shape(w: f32) -> Shape {
    Shape { w, h: 136.0, radius: 22.0, y: 0.0 }
}

/// How wide the open notch needs to be for what it is showing.
pub fn open_width(tab: Tab, lyrics_showing: bool, nothing_listed: bool) -> f32 {
    match tab {
        Tab::Media if lyrics_showing => MEDIA_LYRICS_W,
        Tab::Media => MEDIA_W,
        // With nothing to list it stays the media page's size, so switching tabs moves nothing.
        Tab::Agents | Tab::Calendar if nothing_listed => MEDIA_W,
        Tab::Agents => AGENTS_W,
        Tab::Calendar => CALENDAR_W,
        Tab::Todos | Tab::Shelf => MEDIA_W,
    }
}

/// What the notch should be doing right now. Only the mouse or a question can hold it open,
/// so a busy agent or playing music can never stop it from collapsing.
pub fn target_mode(hovering: bool, needs_decision: bool, flashing: bool, has_tabs: bool, lyric_strip: bool) -> Mode {
    if (hovering && has_tabs) || needs_decision {
        Mode::Open
    } else if flashing {
        Mode::Peek
    } else if lyric_strip {
        Mode::Lyric
    } else {
        Mode::Rest
    }
}

pub struct Motion {
    pub from: Shape,
    pub to: Shape,
    pub elapsed_ms: f32,
}

impl Motion {
    pub fn finished(&self) -> bool {
        self.elapsed_ms >= DURATION_MS
    }

    pub fn current(&self) -> Shape {
        let t = (self.elapsed_ms / DURATION_MS).clamp(0.0, 1.0);
        // Opening overshoots very slightly, closing just settles.
        let growing = self.to.h > self.from.h;
        let eased = if growing {
            let (c1, c3) = (0.9, 1.9);
            1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
        } else {
            1.0 - (1.0 - t).powi(3)
        };
        let mix = |a: f32, b: f32| a + (b - a) * eased;
        Shape { w: mix(self.from.w, self.to.w), h: mix(self.from.h, self.to.h), radius: mix(self.from.radius, self.to.radius), y: mix(self.from.y, self.to.y).max(0.0) }
    }
}

/// How visible the open page is at this height: nothing until most of the opening is done.
pub fn content_alpha(current_h: f32, full_h: f32) -> f32 {
    ((current_h / full_h - 0.55) / 0.45).clamp(0.0, 1.0)
}

/// The most urgent state among the sessions, for the small light on the closed notch.
pub fn light(statuses: impl Iterator<Item = Status>) -> Option<Status> {
    statuses.min_by_key(|s| *s as u8)
}

pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// "just now", "12m ago", "3h ago", "2d ago".
pub fn ago(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "just now".to_string(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

pub fn elapsed(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, s / 60 % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_always_collapses_when_nothing_holds_it() {
        assert_eq!(target_mode(false, false, false, true, false), Mode::Rest);
        assert_eq!(target_mode(true, false, false, true, false), Mode::Open);
        assert_eq!(target_mode(false, true, false, true, false), Mode::Open);
        assert_eq!(target_mode(false, false, true, true, false), Mode::Peek);
    }

    #[test]
    fn hovering_opens_nothing_without_a_feature() {
        assert_eq!(target_mode(true, false, false, false, false), Mode::Rest);
    }

    #[test]
    fn the_lyric_strip_gives_way_to_everything_else() {
        assert_eq!(target_mode(false, false, false, true, true), Mode::Lyric);
        assert_eq!(target_mode(true, false, false, true, true), Mode::Open);
        assert_eq!(target_mode(false, true, false, true, true), Mode::Open);
        assert_eq!(target_mode(false, false, true, true, true), Mode::Peek);
    }

    #[test]
    fn the_lyric_strip_fits_its_line_within_limits() {
        assert_eq!(lyric_shape(0.0).w, LYRIC_MIN_W);
        assert_eq!(lyric_shape(200.0).w, 200.0 + LYRIC_PAD * 2.0);
        assert!(lyric_shape(5000.0).w * 1.03 < CANVAS_W);
        assert!(lyric_shape(100.0).h < shape_for(Mode::Open).h / 4.0);
    }

    #[test]
    fn the_pill_stays_tiny() {
        let pill = shape_for(Mode::Rest);
        assert!(pill.h + pill.y <= 6.0 && pill.w <= 60.0);
    }

    #[test]
    fn tabs_follow_the_switches() {
        assert_eq!(tabs(true, true, true, true, true), vec![Tab::Media, Tab::Agents, Tab::Calendar, Tab::Todos, Tab::Shelf]);
        assert_eq!(tabs(false, true, false, false, true), vec![Tab::Agents, Tab::Shelf]);
        assert!(tabs(false, false, false, false, false).is_empty());
    }

    #[test]
    fn lyrics_off_is_much_narrower() {
        assert!(open_width(Tab::Media, false, false) * 1.5 < open_width(Tab::Media, true, false));
        assert!(open_width(Tab::Media, true, false) * 1.03 < CANVAS_W);
        assert!(open_width(Tab::Agents, false, false) * 1.03 < CANVAS_W);
        assert_eq!(open_width(Tab::Agents, false, true), open_width(Tab::Media, false, false));
    }

    #[test]
    fn every_shape_fits_the_canvas() {
        for mode in [Mode::Rest, Mode::Peek, Mode::Lyric, Mode::Open] {
            let s = shape_for(mode);
            assert!(s.w * 1.03 < CANVAS_W && s.h * 1.1 < CANVAS_H, "{mode:?} {s:?}");
        }
    }

    #[test]
    fn motion_starts_and_ends_exactly() {
        let (from, to) = (shape_for(Mode::Rest), shape_for(Mode::Open));
        assert_eq!(Motion { from, to, elapsed_ms: 0.0 }.current().h.round(), from.h);
        let done = Motion { from, to, elapsed_ms: 500.0 };
        assert!(done.finished());
        assert_eq!(done.current(), to);
        let back = Motion { from: to, to: from, elapsed_ms: 120.0 }.current();
        assert!(back.h < to.h && back.h > from.h);
    }

    #[test]
    fn urgent_light_wins() {
        assert_eq!(light([Status::Done, Status::Running, Status::Waiting].into_iter()), Some(Status::Waiting));
        assert_eq!(light(std::iter::empty()), None);
    }

    #[test]
    fn readable_times() {
        assert_eq!(clock(83_000), "1:23");
        assert_eq!(clock(3_725_000), "1:02:05");
        assert_eq!(elapsed(9), "9s");
        assert_eq!(elapsed(65), "1m 05s");
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(720_000), "12m ago");
        assert_eq!(ago(3 * 3_600_000), "3h ago");
    }
}
