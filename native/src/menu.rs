//! The settings panel you get from right-clicking the notch or its tray icon: its own small window, drawn like the notch.
use crate::gfx::{Align, Gfx, Rect};
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
    Radio { id: usize, label: String, on: bool },
    Button { id: usize, label: String, enabled: bool, danger: bool },
}

impl Row {
    fn height(&self) -> f32 {
        if matches!(self, Row::Header(_)) {
            HEADER_H
        } else {
            ROW_H
        }
    }

    fn id(&self) -> Option<usize> {
        match self {
            Row::Switch { id, enabled: true, .. } | Row::Button { id, enabled: true, .. } | Row::Radio { id, .. } => Some(*id),
            _ => None,
        }
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
}

fn knob_goal(row: &Row) -> f32 {
    match row {
        Row::Switch { on: true, .. } | Row::Radio { on: true, .. } => 1.0,
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
            if hover == Some(i) && row.id().is_some() {
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

    fn activate(&mut self, i: usize) {
        let Some(id) = self.rows.get(i).and_then(Row::id) else { return };
        let main = HWND(crate::WINDOW.load(std::sync::atomic::Ordering::SeqCst) as *mut _);
        let Some(app) = (unsafe { crate::app(main) }) else { return };
        if !shell::menu_choose(app, id) {
            self.close(true);
            return;
        }
        self.rows = shell::menu_rows(app);
        self.knobs.resize(self.rows.len(), 0.0);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SWITCH, SWITCH_FRAME_MS, None);
        }
        self.render();
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
            let colour = if *child { MUTED } else { WHITE };
            g.text(label, Rect { x: r.x + indent, y: r.y, w: r.w - indent - SWITCH_W - 16.0, h: r.h }, 12.0, false, Align::Left, colour, alpha);
            let track = Rect { x: r.x + r.w - SWITCH_W - 8.0, y: r.y + (r.h - SWITCH_H) / 2.0, w: SWITCH_W, h: SWITCH_H };
            g.fill(track, SWITCH_H / 2.0, WHITE, 0.16 * alpha);
            g.fill(track, SWITCH_H / 2.0, BLUE, knob * alpha);
            let travel = SWITCH_W - SWITCH_H;
            g.circle(track.x + SWITCH_H / 2.0 + travel * knob, track.y + SWITCH_H / 2.0, SWITCH_H / 2.0 - 2.0, WHITE, alpha);
        }
        Row::Radio { label, .. } => {
            g.text(label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 40.0, h: r.h }, 12.0, false, Align::Left, WHITE, 1.0);
            let (cx, cy) = (r.x + r.w - 16.0, r.y + r.h / 2.0);
            g.circle(cx, cy, 7.0, WHITE, 0.16);
            g.circle(cx, cy, 7.0, BLUE, knob);
            g.circle(cx, cy, 2.8, WHITE, knob);
        }
        Row::Button { label, enabled, danger, .. } => {
            let colour = if *danger { RED } else { WHITE };
            g.text(label, Rect { x: r.x + 8.0, y: r.y, w: r.w - 16.0, h: r.h }, 12.0, false, Align::Left, colour, if *enabled { 1.0 } else { 0.35 });
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
            Some(i) => m.activate(i),
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
                    m.activate(i);
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
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => {
            let main = HWND(crate::WINDOW.load(std::sync::atomic::Ordering::SeqCst) as *mut _);
            if let Some(app) = crate::app(main) {
                if app.menu == Some(hwnd) {
                    app.menu = None;
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
        let anchor = if from_notch { ((notch.left + notch.right) / 2, notch.top + ((app.shape.y + app.shape.h) * notch_scale) as i32) } else { (cursor.x, cursor.y) };
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
        let m = Box::into_raw(Box::new(Menu { hwnd, gfx, rows, knobs, hover: None, tracking: false, scroll: 0.0, view_h, focus_before: HWND::default() }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, m as isize);
        app.menu = Some(hwnd);
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
            Row::Radio { id: 3, label: "three".into(), on: true },
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
    fn the_panel_stays_on_the_screen() {
        let work = RECT { left: 0, top: 0, right: 1000, bottom: 800 };
        assert_eq!(place((500, 10), true, (300, 400), 8, work), (350, 18));
        assert_eq!(place((990, 790), false, (300, 400), 8, work), (700, 382));
        assert_eq!(place((10, 100), false, (300, 400), 8, work), (0, 108));
        assert_eq!(place((500, 10), true, (300, 900), 8, work).1, 0);
    }
}
