//! The settings panel you get from right-clicking the notch or its tray icon: its own small window, drawn like the notch.
use crate::gfx::{Align, Gfx, Rect};
use crate::glow::{from_hsv, hex, hsv, rgb};
use crate::shell;
use crate::App;
use std::sync::Once;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WIDTH: f32 = 300.0;
const MARGIN: f32 = 14.0; // room around the panel for its shadow
const TITLE_H: f32 = 38.0;
const HEADER_H: f32 = 26.0;
const ROW_H: f32 = 28.0;
const BOTTOM: f32 = 6.0;
const GAP: f32 = 8.0;
const SWITCH_W: f32 = 30.0;
const SWITCH_H: f32 = 17.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_WHEEL: u32 = 0x020A;
const TIMER_SWITCH: usize = 1;
const SWITCH_FRAME_MS: u32 = 16;
const SWITCH_EASE: f32 = 0.35;
const WHEEL_STEP: f32 = 40.0;
const SEGMENT_W: f32 = 66.0;
const SEGMENT_H: f32 = 21.0;
const WHEEL_H: f32 = 148.0;
const WHEEL_R: f32 = 60.0;
const SWATCH_W: f32 = 30.0;
/// How dark the brightness slider lets a color go: any darker and the glow is not there at all.
const DARKEST: f32 = 0.15;
const HUE_STEP: f32 = 12.0;

const PANEL: u32 = 0x18181A;
const WHITE: u32 = 0xFFFFFF;
const MUTED: u32 = 0xA3A7AD;
const FAINT: u32 = 0x6C7076;
const BLUE: u32 = 0x60A5FA;
const RED: u32 = 0xF87171;

#[derive(Clone, PartialEq, Debug)]
pub enum Row {
    Header(&'static str),
    Switch { id: usize, label: String, on: bool, enabled: bool, child: bool },
    /// One of a list; `colors`, when there are any, are shown as a small strip beside it.
    Radio { id: usize, label: String, on: bool, colors: Vec<u32> },
    /// A choice between options shown side by side, one id per option.
    Choice { ids: Vec<usize>, label: String, options: Vec<String>, chosen: usize },
    /// Like a choice, but the options are colors shown as dots.
    Swatches { ids: Vec<usize>, label: String, colors: Vec<u32>, chosen: usize },
    /// A color wheel with a brightness slider, changed by dragging.
    Wheel { id: usize, color: u32 },
    Button { id: usize, label: String, enabled: bool, danger: bool },
}

impl Row {
    fn height(&self) -> f32 {
        match self {
            Row::Header(_) => HEADER_H,
            Row::Wheel { .. } => WHEEL_H,
            _ => ROW_H,
        }
    }

    fn id(&self) -> Option<usize> {
        match self {
            Row::Switch { id, enabled: true, .. } | Row::Button { id, enabled: true, .. } | Row::Radio { id, .. } | Row::Wheel { id, .. } => Some(*id),
            Row::Choice { ids, .. } | Row::Swatches { ids, .. } => ids.first().copied(),
            _ => None,
        }
    }
}

/// Where each option of a choice sits in its row: a strip of equal buttons on the right.
pub fn segments(r: Rect, count: usize) -> Vec<Rect> {
    let left = r.x + r.w - 8.0 - SEGMENT_W * count as f32;
    (0..count).map(|i| Rect { x: left + i as f32 * SEGMENT_W, y: r.y + (r.h - SEGMENT_H) / 2.0, w: SEGMENT_W, h: SEGMENT_H }).collect()
}

/// Where each color of a row of swatches sits: dots on the right.
pub fn swatch_cells(r: Rect, count: usize) -> Vec<Rect> {
    let left = r.x + r.w - 4.0 - SWATCH_W * count as f32;
    (0..count).map(|i| Rect { x: left + i as f32 * SWATCH_W, y: r.y, w: SWATCH_W, h: r.h }).collect()
}

/// The parts of a row that can be clicked one by one, if it has any.
fn cells(row: &Row, r: Rect) -> Vec<Rect> {
    match row {
        Row::Choice { options, .. } => segments(r, options.len()),
        Row::Swatches { colors, .. } => swatch_cells(r, colors.len()),
        _ => vec![],
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Part {
    /// The disc, for the color itself.
    Disc,
    /// The bar beside it, for how bright.
    Bright,
}

/// Where the parts of a color wheel row go: the disc's center, then the slider and the sample of the color.
pub fn wheel_parts(r: Rect) -> ((f32, f32), Rect, Rect) {
    let center = (r.x + 14.0 + WHEEL_R, r.y + r.h / 2.0);
    let slider = Rect { x: center.0 + WHEEL_R + 18.0, y: center.1 - WHEEL_R, w: 14.0, h: WHEEL_R * 2.0 };
    let left = slider.x + slider.w + 18.0;
    let sample = Rect { x: left, y: center.1 - 34.0, w: r.x + r.w - 8.0 - left, h: 44.0 };
    (center, slider, sample)
}

/// Which part of the wheel row a press lands on, with a little room around each.
pub fn wheel_part(r: Rect, x: f32, y: f32) -> Option<Part> {
    let ((cx, cy), slider, _) = wheel_parts(r);
    if (x - cx).hypot(y - cy) <= WHEEL_R + 6.0 {
        Some(Part::Disc)
    } else if (Rect { x: slider.x - 8.0, y: slider.y - 6.0, w: slider.w + 16.0, h: slider.h + 12.0 }).contains(x, y) {
        Some(Part::Bright)
    } else {
        None
    }
}

/// The color the mouse picks on a part of the wheel row, as hue, saturation and brightness.
/// Dragging past the edge holds at the edge.
pub fn pick(part: Part, r: Rect, x: f32, y: f32, (h, s, v): (f32, f32, f32)) -> (f32, f32, f32) {
    let ((cx, cy), slider, _) = wheel_parts(r);
    match part {
        Part::Disc => ((y - cy).atan2(x - cx).to_degrees().rem_euclid(360.0), ((x - cx).hypot(y - cy) / WHEEL_R).min(1.0), v),
        Part::Bright => (h, s, (1.0 - (y - slider.y) / slider.h).clamp(DARKEST, 1.0)),
    }
}

pub fn content_height(rows: &[Row]) -> f32 {
    TITLE_H + rows.iter().map(Row::height).sum::<f32>() + BOTTOM
}

/// Which row is at this height, counted from the top of the content.
pub fn row_at(rows: &[Row], y: f32) -> Option<usize> {
    let mut top = TITLE_H;
    for (i, row) in rows.iter().enumerate() {
        if y >= top && y < top + row.height() {
            return row.id().map(|_| i);
        }
        top += row.height();
    }
    None
}

/// The next row the keyboard can land on, wrapping around.
pub fn next_row(rows: &[Row], from: Option<usize>, down: bool) -> Option<usize> {
    let usable: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].id().is_some()).collect();
    let at = from.and_then(|f| usable.iter().position(|i| *i == f));
    let next = match (at, down) {
        (None, true) => 0,
        (None, false) => usable.len().checked_sub(1)?,
        (Some(a), true) => (a + 1) % usable.len(),
        (Some(a), false) => (a + usable.len() - 1) % usable.len(),
    };
    usable.get(next).copied()
}

struct Menu {
    hwnd: HWND,
    gfx: Option<Gfx>,
    rows: Vec<Row>,
    knobs: Vec<f32>,
    hover: Option<usize>,
    tracking: bool,
    scroll: f32,
    view_h: f32,
    focus_before: HWND,
    /// While the wheel is being dragged: its row, the part, and the color so far.
    /// Kept as hue, saturation and brightness so the hue holds while the color is gray or dark.
    drag: Option<(usize, Part, (f32, f32, f32))>,
}

fn main_app<'a>() -> Option<&'a mut App> {
    let main = HWND(crate::WINDOW.load(std::sync::atomic::Ordering::SeqCst) as *mut _);
    unsafe { crate::app(main) }
}

fn knob_goal(row: &Row) -> f32 {
    match row {
        Row::Switch { on: true, .. } | Row::Radio { on: true, .. } => 1.0,
        // For a choice, the highlight's position: 0 under the first option, 1 under the second, and so on.
        Row::Choice { chosen, .. } | Row::Swatches { chosen, .. } => *chosen as f32,
        _ => 0.0,
    }
}

impl Menu {
    fn max_scroll(&self) -> f32 {
        (content_height(&self.rows) - self.view_h).max(0.0)
    }

    fn panel(&self) -> Rect {
        Rect { x: MARGIN, y: MARGIN, w: WIDTH, h: self.view_h }
    }

    fn render(&mut self) {
        let panel = self.panel();
        let (rows, knobs, hover, scroll) = (&self.rows, &self.knobs, self.hover, self.scroll);
        let Some(g) = self.gfx.as_mut() else { return };
        g.begin();
        for spread in (1..=6).rev() {
            let s = spread as f32 * 2.0;
            g.fill(Rect { x: panel.x - s, y: panel.y - s + 2.0, w: panel.w + s * 2.0, h: panel.h + s * 2.0 }, 14.0 + s, 0x000000, 0.045);
        }
        g.fill(panel, 14.0, WHITE, 0.14);
        g.fill(Rect { x: panel.x + 1.0, y: panel.y + 1.0, w: panel.w - 2.0, h: panel.h - 2.0 }, 13.0, PANEL, 1.0);
        g.clip(Rect { x: panel.x + 1.0, y: panel.y + 1.0, w: panel.w - 2.0, h: panel.h - 2.0 });
        let (x, w) = (panel.x + GAP, panel.w - GAP * 2.0);
        let top = panel.y - scroll;
        g.text("TwoCode", Rect { x: x + 8.0, y: top + 8.0, w: 120.0, h: 24.0 }, 14.0, true, Align::Left, WHITE, 1.0);
        let build = format!("build {}", option_env!("TWOCODE_BUILD").unwrap_or("dev"));
        g.text(&build, Rect { x: x + w - 108.0, y: top + 8.0, w: 100.0, h: 24.0 }, 10.5, false, Align::Right, FAINT, 1.0);
        let mut y = top + TITLE_H;
        for (i, row) in rows.iter().enumerate() {
            let r = Rect { x, y, w, h: row.height() };
            if hover == Some(i) && row.id().is_some() && !matches!(row, Row::Wheel { .. }) {
                g.fill(Rect { y: r.y + 1.0, h: r.h - 2.0, ..r }, 8.0, WHITE, 0.08);
            }
            draw_row(g, row, knobs[i], r);
            y += row.height();
        }
        g.unclip();
        if self.view_h < content_height(rows) {
            let content = content_height(rows);
            let track = panel.h - 16.0;
            g.fill(Rect { x: panel.x + panel.w - 5.0, y: panel.y + 8.0 + track * scroll / content, w: 2.5, h: track * panel.h / content }, 1.2, WHITE, 0.3);
        }
        if let Err(why) = g.end() {
            crate::config::log(&format!("drawing the settings panel failed: {why}"));
        }
    }

    fn animate(&mut self) {
        let mut moving = false;
        for (knob, row) in self.knobs.iter_mut().zip(&self.rows) {
            let goal = knob_goal(row);
            *knob += (goal - *knob) * SWITCH_EASE;
            if (goal - *knob).abs() < 0.01 {
                *knob = goal;
            } else {
                moving = true;
            }
        }
        if !moving {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_SWITCH);
            }
        }
        self.render();
    }

    fn scale(&self) -> f32 {
        self.gfx.as_ref().map_or(1.0, |g| g.scale)
    }

    fn mouse(&self, lparam: LPARAM) -> (f32, f32) {
        let scale = self.scale();
        ((lparam.0 & 0xFFFF) as i16 as f32 / scale, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale)
    }

    /// Where row `i` is drawn right now.
    fn row_rect(&self, i: usize) -> Rect {
        let panel = self.panel();
        let top = panel.y - self.scroll + TITLE_H + self.rows[..i].iter().map(Row::height).sum::<f32>();
        Rect { x: panel.x + GAP, y: top, w: panel.w - GAP * 2.0, h: self.rows[i].height() }
    }

    /// Which option of the choice in row `i` is under the mouse, if any.
    fn option_under(&self, i: usize, lparam: LPARAM) -> Option<usize> {
        let row = self.rows.get(i)?;
        let (x, y) = self.mouse(lparam);
        cells(row, self.row_rect(i)).iter().position(|s| s.contains(x, y))
    }

    /// Starts dragging on the color wheel, if the press is on it.
    fn press(&mut self, lparam: LPARAM) -> bool {
        let Some(i) = self.row_under(lparam) else { return false };
        let Some(Row::Wheel { color, .. }) = self.rows.get(i) else { return false };
        let (x, y) = self.mouse(lparam);
        let Some(part) = wheel_part(self.row_rect(i), x, y) else { return false };
        self.drag = Some((i, part, hsv(rgb(*color))));
        unsafe {
            SetCapture(self.hwnd);
        }
        self.drag_to(lparam);
        true
    }

    fn drag_to(&mut self, lparam: LPARAM) {
        let Some((i, part, was)) = self.drag else { return };
        let (x, y) = self.mouse(lparam);
        let now = pick(part, self.row_rect(i), x, y, was);
        self.drag = Some((i, part, now));
        // Saved when the mouse lets go, not on every move.
        self.set_color(hex(from_hsv(now.0, now.1, now.2)), false);
    }

    /// Changes the color being edited, and the glow with it.
    fn set_color(&mut self, color: u32, save: bool) {
        let Some(app) = main_app() else { return };
        shell::set_glow_color(app, color, save);
        self.refresh(app);
    }

    fn refresh(&mut self, app: &App) {
        let rows = shell::menu_rows(app);
        if rows.len() != self.rows.len() {
            // Rows came or went, so the old switch positions no longer line up: start them where they belong.
            self.knobs = rows.iter().map(knob_goal).collect();
        }
        self.rows = rows;
        self.render();
    }

    fn row_under(&self, lparam: LPARAM) -> Option<usize> {
        let scale = self.scale();
        let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
        let panel = self.panel();
        if !panel.contains(x, y) {
            return None;
        }
        row_at(&self.rows, y - panel.y + self.scroll)
    }

    fn inside(&self, lparam: LPARAM) -> bool {
        let scale = self.scale();
        self.panel().contains((lparam.0 & 0xFFFF) as i16 as f32 / scale, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale)
    }

    /// For a choice, `option` picks one; without it, Enter moves to the next.
    fn activate(&mut self, i: usize, option: Option<usize>) {
        let id = match self.rows.get(i) {
            Some(Row::Choice { ids, chosen, .. } | Row::Swatches { ids, chosen, .. }) => ids.get(option.unwrap_or((chosen + 1) % ids.len())).copied(),
            // The wheel is changed by dragging or with the arrow keys.
            Some(Row::Wheel { .. }) => None,
            Some(row) => row.id(),
            None => None,
        };
        let Some(id) = id else { return };
        let Some(app) = main_app() else { return };
        if !shell::menu_choose(app, id) {
            self.close(true);
            return;
        }
        self.refresh(app);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SWITCH, SWITCH_FRAME_MS, None);
        }
    }

    fn close(&self, restore_focus: bool) {
        unsafe {
            if restore_focus {
                shell::give_focus_back(self.focus_before);
            }
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    fn keep_visible(&mut self, i: usize) {
        let top = TITLE_H + self.rows[..i].iter().map(Row::height).sum::<f32>();
        let bottom = top + self.rows[i].height();
        if top < self.scroll + TITLE_H {
            self.scroll = (top - TITLE_H).max(0.0);
        } else if bottom > self.scroll + self.view_h {
            self.scroll = (bottom - self.view_h).min(self.max_scroll());
        }
    }
}

fn draw_row(g: &mut Gfx, row: &Row, knob: f32, r: Rect) {
    match row {
        Row::Header(title) => {
            g.fill(Rect { x: r.x + 8.0, y: r.y + 2.0, w: r.w - 16.0, h: 1.0 }, 0.0, WHITE, 0.08);
            g.text(title, Rect { x: r.x + 8.0, y: r.y + 5.0, w: r.w - 16.0, h: r.h - 5.0 }, 10.5, true, Align::Left, FAINT, 1.0);
        }
        Row::Switch { label, enabled, child, .. } => {
            let alpha = if *enabled { 1.0 } else { 0.35 };
            let indent = if *child { 16.0 } else { 8.0 };
            let color = if *child { MUTED } else { WHITE };
            g.text(label, Rect { x: r.x + indent, y: r.y, w: r.w - indent - SWITCH_W - 16.0, h: r.h }, 12.0, false, Align::Left, color, alpha);
            let track = Rect { x: r.x + r.w - SWITCH_W - 8.0, y: r.y + (r.h - SWITCH_H) / 2.0, w: SWITCH_W, h: SWITCH_H };
            g.fill(track, SWITCH_H / 2.0, WHITE, 0.16 * alpha);
            g.fill(track, SWITCH_H / 2.0, BLUE, knob * alpha);
            let travel = SWITCH_W - SWITCH_H;
            g.circle(track.x + SWITCH_H / 2.0 + travel * knob, track.y + SWITCH_H / 2.0, SWITCH_H / 2.0 - 2.0, WHITE, alpha);
        }
        Row::Radio { label, colors, .. } => {
            let strip = if colors.is_empty() { 0.0 } else { 62.0 };
            g.text(label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 40.0 - strip, h: r.h }, 12.0, false, Align::Left, WHITE, 1.0);
            let (cx, cy) = (r.x + r.w - 16.0, r.y + r.h / 2.0);
            if !colors.is_empty() {
                g.gradient(Rect { x: cx - 18.0 - 52.0, y: cy - 5.0, w: 52.0, h: 10.0 }, 5.0, colors, false);
            }
            g.circle(cx, cy, 7.0, WHITE, 0.16);
            g.circle(cx, cy, 7.0, BLUE, knob);
            g.circle(cx, cy, 2.8, WHITE, knob);
        }
        Row::Choice { label, options, .. } => {
            g.text(label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0 - SEGMENT_W * options.len() as f32, h: r.h }, 12.0, false, Align::Left, WHITE, 1.0);
            let cells = segments(r, options.len());
            let (first, last) = (cells[0], cells[cells.len() - 1]);
            g.fill(Rect { x: first.x, y: first.y, w: last.x + last.w - first.x, h: first.h }, first.h / 2.0, WHITE, 0.08);
            // The highlight slides between options as the choice changes.
            g.fill(Rect { x: first.x + knob * SEGMENT_W + 2.0, y: first.y + 2.0, w: SEGMENT_W - 4.0, h: first.h - 4.0 }, (first.h - 4.0) / 2.0, BLUE, 1.0);
            for (n, (cell, option)) in cells.iter().zip(options).enumerate() {
                let near = 1.0 - (knob - n as f32).abs().min(1.0);
                g.text(option, *cell, 11.0, near > 0.5, Align::Center, WHITE, 0.55 + 0.45 * near);
            }
        }
        Row::Swatches { label, colors, .. } => {
            let cells = swatch_cells(r, colors.len());
            g.text(label, Rect { x: r.x + 16.0, y: r.y, w: r.w - 24.0 - SWATCH_W * colors.len() as f32, h: r.h }, 12.0, false, Align::Left, MUTED, 1.0);
            for (n, (cell, c)) in cells.iter().zip(colors).enumerate() {
                let (cx, cy) = (cell.x + cell.w / 2.0, cell.y + cell.h / 2.0);
                // A ring that grows in around the one being changed.
                let ring = 1.0 - (knob - n as f32).abs().min(1.0);
                if ring > 0.0 {
                    g.circle(cx, cy, 8.0 + 3.5 * ring, WHITE, ring);
                    g.circle(cx, cy, 8.0 + 2.0 * ring, PANEL, 1.0);
                }
                g.circle(cx, cy, 8.0, *c, 1.0);
            }
        }
        Row::Wheel { color, .. } => {
            let ((cx, cy), slider, sample) = wheel_parts(r);
            let (h, s, v) = hsv(rgb(*color));
            g.circle(cx, cy, WHEEL_R + 1.0, WHITE, 0.14);
            g.hue_wheel(cx, cy, WHEEL_R);
            if v < 1.0 {
                g.circle(cx, cy, WHEEL_R, 0x000000, 1.0 - v);
            }
            let (mx, my) = (cx + h.to_radians().cos() * s * WHEEL_R, cy + h.to_radians().sin() * s * WHEEL_R);
            g.circle(mx, my, 7.5, WHITE, 1.0);
            g.circle(mx, my, 5.5, *color, 1.0);
            g.gradient(slider, slider.w / 2.0, &[hex(from_hsv(h, s, 1.0)), 0x000000], true);
            let at = slider.y + (1.0 - (v - DARKEST) / (1.0 - DARKEST)) * slider.h;
            let at = at.clamp(slider.y + 3.0, slider.y + slider.h - 3.0);
            g.fill(Rect { x: slider.x - 3.0, y: at - 3.0, w: slider.w + 6.0, h: 6.0 }, 3.0, WHITE, 1.0);
            g.fill(sample, 10.0, WHITE, 0.14);
            g.fill(Rect { x: sample.x + 1.0, y: sample.y + 1.0, w: sample.w - 2.0, h: sample.h - 2.0 }, 9.0, *color, 1.0);
            g.text(&format!("#{color:06X}"), Rect { x: sample.x, y: sample.y + sample.h + 6.0, w: sample.w, h: 16.0 }, 11.0, false, Align::Center, MUTED, 1.0);
            g.text("Drag to pick", Rect { x: sample.x, y: sample.y + sample.h + 22.0, w: sample.w, h: 16.0 }, 10.0, false, Align::Center, FAINT, 1.0);
        }
        Row::Button { label, enabled, danger, .. } => {
            let color = if *danger { RED } else { WHITE };
            g.text(label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, 12.0, false, Align::Left, color, if *enabled { 1.0 } else { 0.35 });
        }
    }
}

unsafe fn menu<'a>(hwnd: HWND) -> Option<&'a mut Menu> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Menu).as_mut()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(m) = menu(hwnd) else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            m.render();
        }
        WM_LBUTTONDOWN => {
            m.press(lparam);
        }
        WM_MOUSEMOVE if m.drag.is_some() => m.drag_to(lparam),
        WM_LBUTTONUP if m.drag.is_some() => {
            let _ = ReleaseCapture();
            if let Some((_, _, (h, s, v))) = m.drag.take() {
                m.set_color(hex(from_hsv(h, s, v)), true);
            }
        }
        WM_MOUSEMOVE => {
            if !m.tracking {
                let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut track);
                m.tracking = true;
            }
            let hover = m.row_under(lparam);
            if hover != m.hover {
                m.hover = hover;
                m.render();
            }
        }
        WM_MOUSELEAVE => {
            m.tracking = false;
            if m.hover.take().is_some() {
                m.render();
            }
        }
        WM_LBUTTONUP => match m.row_under(lparam) {
            Some(i) => {
                // A click on a choice's label, rather than an option, does nothing; nor does a click on the wheel.
                let is_choice = matches!(m.rows.get(i), Some(Row::Choice { .. } | Row::Swatches { .. } | Row::Wheel { .. }));
                match m.option_under(i, lparam) {
                    Some(option) => m.activate(i, Some(option)),
                    None if !is_choice => m.activate(i, None),
                    None => {}
                }
            }
            None if !m.inside(lparam) => m.close(false),
            None => {}
        },
        WM_WHEEL => {
            let notches = ((wparam.0 >> 16) as u16 as i16) as f32 / 120.0;
            m.scroll = (m.scroll - notches * WHEEL_STEP).clamp(0.0, m.max_scroll());
            m.render();
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_ESCAPE.0 {
                m.close(true);
            } else if key == VK_DOWN.0 || key == VK_UP.0 {
                m.hover = next_row(&m.rows, m.hover, key == VK_DOWN.0);
                if let Some(i) = m.hover {
                    m.keep_visible(i);
                }
                m.render();
            } else if key == VK_RETURN.0 || key == VK_SPACE.0 {
                if let Some(i) = m.hover {
                    m.activate(i, None);
                }
            } else if key == VK_LEFT.0 || key == VK_RIGHT.0 {
                // Left and Right pick an option of a choice directly, or turn the color wheel.
                let right = key == VK_RIGHT.0;
                match m.hover.and_then(|i| m.rows.get(i).map(|r| (i, r.clone()))) {
                    Some((i, Row::Choice { ids, chosen, .. } | Row::Swatches { ids, chosen, .. })) => {
                        let next = if right { (chosen + 1).min(ids.len() - 1) } else { chosen.saturating_sub(1) };
                        if next != chosen {
                            m.activate(i, Some(next));
                        }
                    }
                    Some((_, Row::Wheel { color, .. })) => {
                        let (h, s, v) = hsv(rgb(color));
                        m.set_color(hex(from_hsv(h + if right { HUE_STEP } else { -HUE_STEP }, s, v)), true);
                    }
                    _ => {}
                }
            }
        }
        WM_TIMER => m.animate(),
        WM_ACTIVATE => {
            // Clicking anywhere else closes it, and that window keeps the focus.
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                m.close(false);
            }
        }
        WM_CLOSE => {
            if m.drag.take().is_some() {
                let _ = ReleaseCapture();
            }
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => {
            let main = HWND(crate::WINDOW.load(std::sync::atomic::Ordering::SeqCst) as *mut _);
            if let Some(app) = crate::app(main) {
                if app.menu == Some(hwnd) {
                    app.menu = None;
                    // The notch was waiting for this, unless the mouse is on it.
                    app.menu_holds_open = false;
                    app.settle();
                }
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(m as *mut Menu));
        }
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    LRESULT(0)
}

/// Where the panel's own rectangle goes, in pixels, kept inside the screen's working area.
pub fn place(anchor: (i32, i32), below: bool, size: (i32, i32), gap: i32, work: RECT) -> (i32, i32) {
    let x = (anchor.0 - size.0 / 2).clamp(work.left, (work.right - size.0).max(work.left));
    let wanted = if below || anchor.1 + gap + size.1 <= work.bottom { anchor.1 + gap } else { anchor.1 - gap - size.1 };
    (x, wanted.clamp(work.top, (work.bottom - size.1).max(work.top)))
}

/// Opens the panel, or closes it if it is already open. `from_notch` puts it under the notch instead of at the mouse.
pub fn open(app: &mut App, from_notch: bool) {
    if let Some(open) = app.menu {
        unsafe {
            let _ = DestroyWindow(open);
        }
        return;
    }
    static REGISTER: Once = Once::new();
    let class = w!("TwoCodeMenu");
    unsafe {
        let Ok(instance) = GetModuleHandleW(PCWSTR::null()) else { return };
        REGISTER.call_once(|| {
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() });
        });
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let mut notch = RECT::default();
        let _ = GetWindowRect(app.hwnd, &mut notch);
        let notch_scale = app.gfx.as_ref().map_or(1.0, |g| g.scale);
        // Under the open notch, even if it is only starting to open.
        let open_h = crate::layout::shape_for(crate::layout::Mode::Open).h;
        let anchor = if from_notch { ((notch.left + notch.right) / 2, notch.top + (open_h * notch_scale) as i32) } else { (cursor.x, cursor.y) };
        let monitor = MonitorFromPoint(POINT { x: anchor.0, y: anchor.1 }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x.max(96) as f32 / 96.0;
        let rows = shell::menu_rows(app);
        let work = info.rcWork;
        let room = (work.bottom - work.top) as f32 / scale - 16.0;
        let view_h = content_height(&rows).min(room);
        let panel = ((WIDTH * scale) as i32, (view_h * scale) as i32);
        let (px, py) = place(anchor, from_notch, panel, (GAP * scale) as i32, work);
        let margin = (MARGIN * scale) as i32;
        let (w, h) = (panel.0 + margin * 2, panel.1 + margin * 2);
        let style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP;
        let Ok(hwnd) = CreateWindowExW(style, class, w!("TwoCode settings"), WS_POPUP, px - margin, py - margin, w, h, None, None, Some(instance.into()), None) else { return };
        let gfx = match Gfx::new(hwnd, w as u32, h as u32, scale) {
            Ok(gfx) => Some(gfx),
            Err(why) => {
                crate::config::log(&format!("the settings panel could not start: {why}"));
                let _ = DestroyWindow(hwnd);
                return;
            }
        };
        let knobs = rows.iter().map(knob_goal).collect();
        let m = Box::into_raw(Box::new(Menu { hwnd, gfx, rows, knobs, hover: None, tracking: false, scroll: 0.0, view_h, focus_before: HWND::default(), drag: None }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, m as isize);
        app.menu = Some(hwnd);
        app.menu_holds_open = from_notch;
        app.settle();
        (*m).render();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let before = shell::take_focus(hwnd);
        (*m).focus_before = if before == hwnd || before == app.hwnd { HWND::default() } else { before };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            Row::Header("A"),
            Row::Switch { id: 1, label: "one".into(), on: true, enabled: true, child: false },
            Row::Switch { id: 2, label: "off".into(), on: false, enabled: false, child: true },
            Row::Radio { id: 3, label: "three".into(), on: true, colors: vec![] },
            Row::Header("B"),
            Row::Button { id: 4, label: "four".into(), enabled: true, danger: true },
        ]
    }

    #[test]
    fn rows_are_found_by_height_and_only_usable_ones_count() {
        let list = rows();
        assert_eq!(row_at(&list, 0.0), None); // the title
        assert_eq!(row_at(&list, TITLE_H + 1.0), None); // a section heading
        assert_eq!(row_at(&list, TITLE_H + HEADER_H + 1.0), Some(1));
        assert_eq!(row_at(&list, TITLE_H + HEADER_H + ROW_H + 1.0), None); // switched off
        assert_eq!(row_at(&list, TITLE_H + HEADER_H + ROW_H * 2.0 + 1.0), Some(3));
        assert_eq!(row_at(&list, content_height(&list) + 5.0), None);
    }

    #[test]
    fn the_keyboard_skips_headings_and_disabled_rows_and_wraps() {
        let list = rows();
        assert_eq!(next_row(&list, None, true), Some(1));
        assert_eq!(next_row(&list, Some(1), true), Some(3));
        assert_eq!(next_row(&list, Some(3), true), Some(5));
        assert_eq!(next_row(&list, Some(5), true), Some(1));
        assert_eq!(next_row(&list, Some(1), false), Some(5));
        assert_eq!(next_row(&list, None, false), Some(5));
        assert_eq!(next_row(&[Row::Header("x")], None, true), None);
    }

    #[test]
    fn choice_options_sit_side_by_side_on_the_right() {
        let row = Rect { x: 0.0, y: 100.0, w: 284.0, h: ROW_H };
        let cells = segments(row, 2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].x + cells[1].w, 284.0 - 8.0);
        assert_eq!(cells[1].x, cells[0].x + SEGMENT_W);
        assert!(cells[0].contains(cells[0].x + 1.0, 110.0) && !cells[1].contains(cells[0].x + 1.0, 110.0));
        let choice = Row::Choice { ids: vec![7, 8], label: "x".into(), options: vec!["a".into(), "b".into()], chosen: 1 };
        assert_eq!((choice.id(), knob_goal(&choice)), (Some(7), 1.0));
    }

    #[test]
    fn the_wheel_picks_hue_from_the_angle_and_paleness_from_the_middle() {
        let r = Rect { x: 0.0, y: 0.0, w: 284.0, h: WHEEL_H };
        let ((cx, cy), slider, _) = wheel_parts(r);
        assert_eq!(wheel_part(r, cx, cy), Some(Part::Disc));
        assert_eq!(wheel_part(r, slider.x + 2.0, slider.y + 10.0), Some(Part::Bright));
        assert_eq!(wheel_part(r, r.w - 4.0, cy), None);
        // Straight right is red at full color, straight down a quarter turn on.
        let (h, s, v) = pick(Part::Disc, r, cx + WHEEL_R, cy, (0.0, 0.0, 0.7));
        assert!(h.abs() < 0.01 && (s - 1.0).abs() < 0.01 && v == 0.7);
        assert!((pick(Part::Disc, r, cx, cy + 20.0, (0.0, 0.0, 1.0)).0 - 90.0).abs() < 0.01);
        // Past the rim holds at full color.
        assert_eq!(pick(Part::Disc, r, cx + 500.0, cy, (0.0, 0.0, 1.0)).1, 1.0);
        // The top of the slider is full brightness, and the bottom stops before black.
        assert_eq!(pick(Part::Bright, r, slider.x, slider.y - 40.0, (10.0, 0.5, 0.5)), (10.0, 0.5, 1.0));
        assert_eq!(pick(Part::Bright, r, slider.x, slider.y + 900.0, (10.0, 0.5, 0.5)).2, DARKEST);
    }

    #[test]
    fn swatches_sit_on_the_right_and_can_be_clicked() {
        let r = Rect { x: 0.0, y: 0.0, w: 284.0, h: ROW_H };
        let row = Row::Swatches { ids: vec![1, 2, 3], label: "x".into(), colors: vec![0, 0, 0], chosen: 2 };
        let found = cells(&row, r);
        assert_eq!(found.len(), 3);
        assert_eq!(found[2].x + found[2].w, 284.0 - 4.0);
        assert_eq!((row.id(), knob_goal(&row)), (Some(1), 2.0));
        assert_eq!(Row::Wheel { id: 9, color: 0 }.height(), WHEEL_H);
    }

    #[test]
    fn the_panel_stays_on_the_screen() {
        let work = RECT { left: 0, top: 0, right: 1000, bottom: 800 };
        assert_eq!(place((500, 10), true, (300, 400), 8, work), (350, 18));
        assert_eq!(place((990, 790), false, (300, 400), 8, work), (700, 382));
        assert_eq!(place((10, 100), false, (300, 400), 8, work), (0, 108));
        assert_eq!(place((500, 10), true, (300, 900), 8, work).1, 0);
    }
}
