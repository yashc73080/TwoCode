//! The notch's sizes and motion. Measured in device-independent pixels; no Windows calls, so it can be tested.
use crate::agents::Status;

pub const CANVAS_W: f32 = 720.0;
pub const CANVAS_H: f32 = 240.0;
const DURATION_MS: f32 = 240.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Rest,
    Peek,
    Open,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shape {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

pub struct Modules {
    pub media: bool,
    pub agents: bool,
    pub minimal: bool,
}

pub fn shape_for(mode: Mode, m: &Modules) -> Shape {
    match mode {
        Mode::Rest if m.minimal => Shape { w: 190.0, h: 5.0, radius: 3.0 },
        Mode::Rest => Shape { w: 150.0, h: 26.0, radius: 11.0 },
        Mode::Peek => Shape { w: 340.0, h: 40.0, radius: 16.0 },
        Mode::Open => match (m.media, m.agents) {
            (true, true) => Shape { w: 680.0, h: 184.0, radius: 26.0 },
            (true, false) => Shape { w: 350.0, h: 184.0, radius: 26.0 },
            _ => Shape { w: 410.0, h: 184.0, radius: 26.0 },
        },
    }
}

/// What the notch should be doing right now. Only the mouse or a question can hold it open,
/// so a busy agent or playing music can never stop it from collapsing.
pub fn target_mode(hovering: bool, needs_decision: bool, flashing: bool) -> Mode {
    if hovering || needs_decision {
        Mode::Open
    } else if flashing {
        Mode::Peek
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
        Shape { w: mix(self.from.w, self.to.w), h: mix(self.from.h, self.to.h), radius: mix(self.from.radius, self.to.radius) }
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
        assert_eq!(target_mode(false, false, false), Mode::Rest);
        assert_eq!(target_mode(true, false, false), Mode::Open);
        assert_eq!(target_mode(false, true, false), Mode::Open);
        assert_eq!(target_mode(false, false, true), Mode::Peek);
    }

    #[test]
    fn every_shape_fits_the_canvas() {
        for (media, agents) in [(true, true), (true, false), (false, true)] {
            for minimal in [false, true] {
                let m = Modules { media, agents, minimal };
                for mode in [Mode::Rest, Mode::Peek, Mode::Open] {
                    let s = shape_for(mode, &m);
                    assert!(s.w * 1.03 < CANVAS_W && s.h * 1.1 < CANVAS_H, "{mode:?} {s:?}");
                }
            }
        }
    }

    #[test]
    fn motion_starts_and_ends_exactly() {
        let m = Modules { media: true, agents: true, minimal: false };
        let (from, to) = (shape_for(Mode::Rest, &m), shape_for(Mode::Open, &m));
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
    }
}
