//! A soft light around the edges of the screen, colored like the album cover or a palette you pick.
//! Its own window covers the display, lets every click through, and sits just under the notch. It is drawn at a
//! third of the screen's resolution and stretched, since a glow has no sharp detail, and blurred on the graphics card.
//! While nothing moves it draws nothing; switched off or faded out, the window hides and lets go of its surface.
use crate::audio::Levels;
use crate::gfx::{d2d_rect, Gfx};
use serde::{Deserialize, Serialize};
use std::sync::Once;
use std::time::Instant;
use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The surface is this many times smaller than the screen in each direction.
const STRETCH: f32 = 3.0;
/// How many pieces each side's color changes over. The blur hides the steps.
const PIECES: usize = 96;
const FADE_PER_S: f32 = 2.5;
const BLEND_PER_S: f32 = 2.0;
/// The colors around the screen, as evenly spaced samples: one, two or three chosen colors all fit exactly.
const SAMPLES: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Palette {
    #[default]
    Album,
    Sunset,
    Ocean,
    Aurora,
    /// The colors picked on the wheel in the settings.
    Custom,
}

impl Palette {
    pub const ALL: [Palette; 5] = [Palette::Album, Palette::Sunset, Palette::Ocean, Palette::Aurora, Palette::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Palette::Album => "Album cover colors",
            Palette::Sunset => "Sunset",
            Palette::Ocean => "Ocean",
            Palette::Aurora => "Aurora",
            Palette::Custom => "Your own colors",
        }
    }

    /// A preset's colors. None for those that come from elsewhere.
    pub fn colors(self) -> Option<[u32; 3]> {
        match self {
            Palette::Album | Palette::Custom => None,
            Palette::Sunset => Some([0xFF4D6D, 0xFFA94D, 0xC05CFF]),
            Palette::Ocean => Some([0x00C6FF, 0x2F6BFF, 0x7B5CFF]),
            Palette::Aurora => Some([0x00F5A0, 0x00C8F5, 0xA45CFF]),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Motion {
    /// Holds still, and only changes when the song does.
    Still,
    /// The colors travel slowly around the screen.
    Drift,
    /// Brighter and wider with the beat, and the colors travel faster when it is loud.
    #[default]
    Music,
}

/// How the colors are laid around the screen.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Style {
    /// Through every color once on the way round.
    #[default]
    Around,
    /// The same on the left and right: the first color at the top, the last at the bottom.
    Mirror,
    /// The whole edge one color at a time, fading from one to the next.
    Fade,
}

pub struct Settings {
    pub palette: Palette,
    /// The colors for `Palette::Custom`, one to three of them.
    pub custom: Vec<u32>,
    pub style: Style,
    pub motion: Motion,
    /// 0 thin, 1 medium, 2 thick.
    pub size: u8,
    /// 0 soft, 1 medium, 2 bright.
    pub strength: u8,
}

type Rgb = [f32; 3];

pub(crate) fn rgb(hex: u32) -> Rgb {
    [((hex >> 16) & 255) as f32 / 255.0, ((hex >> 8) & 255) as f32 / 255.0, (hex & 255) as f32 / 255.0]
}

pub(crate) fn hsv(c: Rgb) -> (f32, f32, f32) {
    let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

pub(crate) fn from_hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// Light needs to be vivid to read as color on a dark screen: keep the hue, lift the rest.
fn vivid(c: Rgb) -> Rgb {
    let (h, s, v) = hsv(c);
    from_hsv(h, s.max(0.6).min(0.95), v.max(0.92))
}

/// The cover's strongest colors, as three for the glow to travel through.
/// Colors are grouped by hue and weighted by how colorful and bright they are, so a large dark or gray
/// background does not win over a small bright subject. A gray cover gives a soft white.
pub fn pick_colors(pixels: &[Rgb]) -> [Rgb; 3] {
    const BUCKETS: usize = 18;
    let mut weight = [0.0f32; BUCKETS];
    let mut sum = [[0.0f32; 3]; BUCKETS];
    for p in pixels {
        let (h, s, v) = hsv(*p);
        if s < 0.18 || v < 0.15 {
            continue;
        }
        let b = ((h / 360.0 * BUCKETS as f32) as usize).min(BUCKETS - 1);
        let w = s * v * v;
        weight[b] += w;
        for i in 0..3 {
            sum[b][i] += p[i] * w;
        }
    }
    let total: f32 = weight.iter().sum();
    let mut order: Vec<usize> = (0..BUCKETS).collect();
    order.sort_by(|a, b| weight[*b].total_cmp(&weight[*a]));
    let mut chosen: Vec<(usize, Rgb)> = vec![];
    for b in order {
        if weight[b] <= 0.0 || weight[b] < total * 0.04 || chosen.len() == 3 {
            break;
        }
        // Neighbouring hues are one color; the next pick should look different.
        let apart = chosen.iter().all(|(c, _)| {
            let d = (*c as isize - b as isize).unsigned_abs();
            d.min(BUCKETS - d) >= 2
        });
        if apart {
            chosen.push((b, sum[b].map(|x| x / weight[b])));
        }
    }
    let colors: Vec<Rgb> = chosen.into_iter().map(|(_, c)| vivid(c)).collect();
    match colors.as_slice() {
        [] => [[0.92, 0.93, 1.0], [0.80, 0.84, 1.0], [1.0, 0.95, 0.92]],
        [one] => {
            let (h, s, v) = hsv(*one);
            [*one, from_hsv(h + 28.0, s, v), from_hsv(h - 28.0, s, v)]
        }
        [a, b] => {
            let (h, s, v) = hsv(*a);
            [*a, *b, from_hsv(h + 20.0, s * 0.8, v)]
        }
        [a, b, c, ..] => [*a, *b, *c],
    }
}

pub(crate) fn hex(c: Rgb) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(c[0]) << 16) | (byte(c[1]) << 8) | byte(c[2])
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Any number of colors as a loop of evenly spaced samples, easing smoothly from each color to the next.
fn spread(stops: &[Rgb]) -> [Rgb; SAMPLES] {
    let Some(first) = stops.first() else { return [[1.0; 3]; SAMPLES] };
    let n = stops.len();
    std::array::from_fn(|k| {
        let x = k as f32 / SAMPLES as f32 * n as f32;
        let i = (x as usize).min(n - 1);
        let t = x - i as f32;
        let t = t * t * (3.0 - 2.0 * t);
        if n == 1 {
            *first
        } else {
            mix(stops[i], stops[(i + 1) % n], t)
        }
    })
}

/// The color at `at` around the loop, 0 to 1, passing smoothly through every sample.
fn color_at(colors: &[Rgb; SAMPLES], at: f32) -> Rgb {
    let x = at.rem_euclid(1.0) * SAMPLES as f32;
    let i = (x as usize).min(SAMPLES - 1);
    mix(colors[i], colors[(i + 1) % SAMPLES], x - i as f32)
}

/// How loud the music is from the waveform's bars, leaning on the low ones where the beat is.
fn loudness(bars: &Levels) -> f32 {
    let low = bars[..3].iter().sum::<f32>() / 3.0;
    let all = bars.iter().sum::<f32>() / bars.len() as f32;
    (low * 0.65 + all * 0.35).clamp(0.0, 1.0)
}

fn ease(from: f32, to: f32, per_s: f32, dt: f32) -> f32 {
    from + (to - from) * (per_s * dt).min(1.0)
}

struct Surface {
    gfx: Gfx,
    layer: ID2D1Bitmap1,
    blur: ID2D1Effect,
    wide: ID2D1Effect,
    /// The haze, the glow and the line, each through an opacity effect, in drawing order.
    fades: [ID2D1Effect; 3],
    brush: ID2D1SolidColorBrush,
    size: (f32, f32),
}

pub struct Glow {
    hwnd: HWND,
    surface: Option<Surface>,
    area: RECT,
    scale: f32,
    wanted: bool,
    presence: f32,
    colors: [Rgb; SAMPLES],
    target: [Rgb; SAMPLES],
    /// How many colors the target is made of.
    count: usize,
    album: Option<[Rgb; 3]>,
    art_rev: Option<u64>,
    settings: Settings,
    phase: f32,
    level: f32,
    last: Instant,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            // A layered window keeps asking to be painted until a real paint pass ends; the frames go through composition.
            use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
            let mut paint = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl Glow {
    pub fn new() -> Glow {
        static REGISTER: Once = Once::new();
        let class = w!("TwoCodeGlow");
        let hwnd = unsafe {
            let instance = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
            REGISTER.call_once(|| {
                RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() });
            });
            // Layered and transparent: Windows sends every click and hover to whatever is underneath.
            let style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT;
            let hwnd = CreateWindowExW(style, class, w!("TwoCode glow"), WS_POPUP, 0, 0, 10, 10, None, None, Some(instance.into()), None).unwrap_or_default();
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            hwnd
        };
        let start = spread(&pick_colors(&[]));
        Glow {
            hwnd,
            surface: None,
            area: RECT::default(),
            scale: 1.0,
            wanted: false,
            presence: 0.0,
            colors: start,
            target: start,
            count: 3,
            album: None,
            art_rev: None,
            settings: Settings { palette: Palette::Album, custom: vec![], style: Style::Around, motion: Motion::Music, size: 1, strength: 1 },
            phase: 0.0,
            level: 0.0,
            last: Instant::now(),
        }
    }

    /// Reads the cover's colors once per track. Cheap: the cover is shrunk to a handful of pixels first.
    pub fn set_art(&mut self, rev: u64, bytes: Option<&[u8]>) {
        if self.art_rev == Some(rev) {
            return;
        }
        self.art_rev = Some(rev);
        self.album = bytes.and_then(|b| cover_pixels(b).ok()).map(|p| pick_colors(&p));
    }

    /// The colors taken from the current cover, for the settings to show.
    pub fn album_colors(&self) -> Vec<u32> {
        self.album.map(|c| c.iter().map(|x| hex(*x)).collect()).unwrap_or_default()
    }

    /// Says what the glow should look like now. True if it has frames to draw.
    pub fn update(&mut self, settings: Settings, wanted: bool, area: RECT, below: HWND) -> bool {
        let stops: Vec<Rgb> = match settings.palette {
            Palette::Album => self.album.unwrap_or_else(|| pick_colors(&[])).to_vec(),
            Palette::Custom if !settings.custom.is_empty() => settings.custom.iter().take(3).map(|c| rgb(*c)).collect(),
            Palette::Custom => vec![[1.0; 3]],
            preset => preset.colors().unwrap_or_default().map(rgb).to_vec(),
        };
        self.count = stops.len();
        self.target = spread(&stops);
        self.settings = settings;
        let usable = area.right > area.left && area.bottom > area.top;
        let wanted = wanted && usable;
        if wanted && (self.surface.is_none() || area != self.area) {
            self.area = area;
            self.build(below);
        }
        if wanted && !self.wanted {
            self.last = Instant::now();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        if wanted {
            self.keep_under(below);
        }
        self.wanted = wanted && self.surface.is_some();
        self.needs_frames()
    }

    /// Back under the notch: anything raised to the top since then would otherwise cover it.
    fn keep_under(&self, notch: HWND) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(notch), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    fn needs_frames(&self) -> bool {
        let presence_goal = if self.wanted { 1.0 } else { 0.0 };
        let settled = self.colors.iter().zip(&self.target).all(|(a, b)| (0..3).all(|i| (a[i] - b[i]).abs() < 0.004));
        self.surface.is_some() && (self.presence != presence_goal || !settled || (self.wanted && self.settings.motion != Motion::Still))
    }

    fn build(&mut self, below: HWND) {
        self.surface = None;
        let a = self.area;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(below), a.left, a.top, a.right - a.left, a.bottom - a.top, SWP_NOACTIVATE);
        }
        self.scale = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0;
        let (w, h) = (((a.right - a.left) as f32 / STRETCH).ceil() as u32, ((a.bottom - a.top) as f32 / STRETCH).ceil() as u32);
        match make_surface(self.hwnd, w, h) {
            Ok(surface) => self.surface = Some(surface),
            Err(why) => crate::config::log(&format!("the screen glow could not start: {why}")),
        }
    }

    /// Draws one frame. False once there is nothing left to animate.
    pub fn frame(&mut self, bars: &Levels) -> bool {
        let dt = self.last.elapsed().as_secs_f32().min(0.1);
        self.last = Instant::now();
        let goal = if self.wanted { 1.0 } else { 0.0 };
        self.presence = if (self.presence - goal).abs() < FADE_PER_S * dt { goal } else { self.presence + (goal - self.presence).signum() * FADE_PER_S * dt };
        for (c, t) in self.colors.iter_mut().zip(&self.target) {
            for i in 0..3 {
                c[i] = ease(c[i], t[i], BLEND_PER_S, dt);
                if (c[i] - t[i]).abs() < 0.004 {
                    c[i] = t[i];
                }
            }
        }
        let music = self.settings.motion == Motion::Music;
        let loud = if music { loudness(bars) } else { 0.0 };
        // Jumps up with a beat and falls back more slowly, so it pulses instead of flickering.
        self.level = if loud > self.level { ease(self.level, loud, 18.0, dt) } else { ease(self.level, loud, 4.0, dt) };
        self.phase += dt
            * match self.settings.motion {
                Motion::Still => 0.0,
                Motion::Drift => 0.035,
                Motion::Music => 0.02 + 0.12 * self.level,
            }
            // Fading changes the whole edge at once, so it has to move further to be seen.
            * if self.settings.style == Style::Fade { 2.5 } else { 1.0 };
        if self.presence == 0.0 && !self.wanted {
            // Faded out: give back the window and the memory behind it.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.surface = None;
            return false;
        }
        if let Err(why) = self.draw() {
            crate::config::log(&format!("drawing the screen glow failed: {why}"));
            self.surface = None;
            return false;
        }
        self.needs_frames()
    }

    fn draw(&self) -> Result<()> {
        let Some(s) = &self.surface else { return Ok(()) };
        let px = self.scale / STRETCH; // screen points to surface pixels
        let thick = [3.0, 6.0, 10.0][self.settings.size.min(2) as usize];
        let strength = [0.55, 0.8, 1.0][self.settings.strength.min(2) as usize];
        let t = thick * px * (1.0 + 0.7 * self.level);
        let alpha = self.presence * strength * if self.settings.motion == Motion::Music { 0.6 + 0.4 * self.level } else { 1.0 };
        let (w, h) = s.size;
        let dc = s.gfx.dc();
        unsafe {
            // The edges as solid bands of color, changing a little each piece.
            dc.SetTarget(&s.layer);
            dc.BeginDraw();
            dc.SetTransform(&crate::gfx::matrix(1.0, 0.0, 0.0));
            dc.Clear(Some(&crate::gfx::color(0, 0.0)));
            let perimeter = 2.0 * (w + h);
            for i in 0..PIECES {
                let (from, to) = (i as f32 / PIECES as f32, (i + 1) as f32 / PIECES as f32);
                let c = color_at(&self.colors, place((from + to) / 2.0, self.settings.style, self.count, w / perimeter, self.phase));
                s.brush.SetColor(&D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: 1.0 });
                for r in band(from * perimeter, to * perimeter, w, h, t) {
                    dc.FillRectangle(&d2d_rect(r), &s.brush);
                }
            }
            dc.EndDraw(None, None)?;
            // A wide soft haze, a tighter glow, then the bright line itself.
            set_float(&s.wide, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, t * 6.0 + 8.0 * px)?;
            set_float(&s.blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, t * 1.6 + 2.0 * px)?;
            dc.SetTarget(s.gfx.target());
            dc.BeginDraw();
            dc.Clear(Some(&crate::gfx::color(0, 0.0)));
            for (fade, opacity) in s.fades.iter().zip([0.7, 0.9, 0.45]) {
                set_float(fade, D2D1_OPACITY_PROP_OPACITY.0 as u32, (opacity * alpha).clamp(0.0, 1.0))?;
                dc.DrawImage(&fade.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            }
            s.gfx.end_now()
        }
    }
}

/// Where on the loop of colors the edge at `at` takes its color from, for each style.
/// `top` is how much of the way round the top edge takes.
fn place(at: f32, style: Style, count: usize, top: f32, phase: f32) -> f32 {
    match style {
        Style::Around => at + phase,
        Style::Mirror => {
            // 0 at the middle of the top, 1 at the middle of the bottom, the same down both sides.
            let d = (at - top / 2.0).rem_euclid(1.0);
            let down = d.min(1.0 - d) * 2.0;
            let n = count.max(1) as f32;
            down * (n - 1.0) / n + phase
        }
        Style::Fade => phase + at * 0.04,
    }
}

unsafe fn set_float(effect: &ID2D1Effect, index: u32, value: f32) -> Result<()> {
    effect.SetValue(index, D2D1_PROPERTY_TYPE_FLOAT, &value.to_le_bytes())
}

/// The part of the screen's edge between `from` and `to`, measured clockwise from the top left, as bands `t` deep.
fn band(from: f32, to: f32, w: f32, h: f32, t: f32) -> Vec<crate::gfx::Rect> {
    use crate::gfx::Rect;
    let mut out = vec![];
    // Each side, where it starts along the way round, and how to turn a stretch of it into a band.
    let sides: [(f32, f32, &dyn Fn(f32, f32) -> Rect); 4] = [
        (0.0, w, &|a, b| Rect { x: a, y: 0.0, w: b - a, h: t }),
        (w, h, &|a, b| Rect { x: w - t, y: a, w: t, h: b - a }),
        (w + h, w, &|a, b| Rect { x: w - b, y: h - t, w: b - a, h: t }),
        (2.0 * w + h, h, &|a, b| Rect { x: 0.0, y: h - b, w: t, h: b - a }),
    ];
    for (start, length, make) in sides {
        let (a, b) = ((from - start).max(0.0), (to - start).min(length));
        if b > a {
            out.push(make(a, b));
        }
    }
    out
}

fn make_surface(hwnd: HWND, w: u32, h: u32) -> Result<Surface> {
    let gfx = Gfx::stretched(hwnd, w, h, 1.0, STRETCH)?;
    unsafe {
        let dc = gfx.dc();
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
            ..Default::default()
        };
        let layer = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props)?;
        let make_blur = || -> Result<ID2D1Effect> {
            let blur = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
            blur.SetInput(0, &layer, true);
            // Hard edges: the light stays bright at the screen's edge instead of fading into what is beyond it.
            blur.SetValue(D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32, D2D1_PROPERTY_TYPE_ENUM, &(D2D1_BORDER_MODE_HARD.0 as u32).to_le_bytes())?;
            blur.SetValue(D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32, D2D1_PROPERTY_TYPE_ENUM, &(D2D1_GAUSSIANBLUR_OPTIMIZATION_SPEED.0 as u32).to_le_bytes())?;
            Ok(blur)
        };
        let (blur, wide) = (make_blur()?, make_blur()?);
        let fade = |image: &ID2D1Image| -> Result<ID2D1Effect> {
            let effect = dc.CreateEffect(&CLSID_D2D1Opacity)?;
            effect.SetInput(0, image, true);
            Ok(effect)
        };
        let fades = [fade(&wide.GetOutput()?)?, fade(&blur.GetOutput()?)?, fade(&layer.cast()?)?];
        let brush = dc.CreateSolidColorBrush(&crate::gfx::color(0xFFFFFF, 1.0), None)?;
        Ok(Surface { gfx, layer, blur, wide, fades, brush, size: (w as f32, h as f32) })
    }
}

/// The cover shrunk to 24 by 24, as colors.
fn cover_pixels(bytes: &[u8]) -> Result<Vec<Rgb>> {
    const SIDE: u32 = 24;
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = SHCreateMemStream(Some(bytes)).ok_or_else(windows::core::Error::empty)?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&frame, SIDE, SIDE, WICBitmapInterpolationModeFant)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(&scaler, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut data = vec![0u8; (SIDE * SIDE * 4) as usize];
        converter.CopyPixels(std::ptr::null(), SIDE * 4, &mut data)?;
        Ok(data.chunks(4).map(|p| [p[2] as f32 / 255.0, p[1] as f32 / 255.0, p[0] as f32 / 255.0]).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue(c: Rgb) -> f32 {
        hsv(c).0
    }

    #[test]
    fn a_red_and_blue_cover_glows_red_and_blue() {
        let mut pixels = vec![[0.85, 0.1, 0.1]; 300];
        pixels.extend(vec![[0.1, 0.2, 0.9]; 200]);
        let c = pick_colors(&pixels);
        assert!(hue(c[0]) < 15.0 || hue(c[0]) > 345.0, "{c:?}");
        assert!((200.0..250.0).contains(&hue(c[1])), "{c:?}");
    }

    #[test]
    fn a_dark_background_does_not_beat_a_bright_subject() {
        let mut pixels = vec![[0.05, 0.05, 0.08]; 900];
        pixels.extend(vec![[0.2, 0.25, 0.2]; 200]); // dull and gray
        pixels.extend(vec![[1.0, 0.75, 0.1]; 60]);
        let c = pick_colors(&pixels);
        assert!((30.0..55.0).contains(&hue(c[0])), "{c:?}");
    }

    #[test]
    fn a_gray_cover_gives_soft_white() {
        let c = pick_colors(&vec![[0.5, 0.5, 0.5]; 100]);
        assert!(c.iter().all(|x| x.iter().all(|v| *v > 0.75)));
    }

    #[test]
    fn colors_are_made_bright_enough_to_be_light() {
        let c = pick_colors(&vec![[0.3, 0.05, 0.3]; 100]);
        assert!(hsv(c[0]).2 > 0.9);
    }

    #[test]
    fn the_color_wraps_smoothly_around_the_screen() {
        let c = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let around = spread(&c);
        assert_eq!(color_at(&around, 0.0), c[0]);
        assert_eq!(color_at(&around, 1.0 / 3.0), c[1]);
        let near_end = color_at(&around, 0.9999);
        assert!((near_end[0] - 1.0).abs() < 0.01);
    }

    #[test]
    fn one_or_two_colors_fill_the_loop_evenly() {
        let one = spread(&[[0.2, 0.4, 0.6]]);
        assert!(one.iter().all(|c| *c == [0.2, 0.4, 0.6]));
        let two = spread(&[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!((two[0], two[SAMPLES / 2]), ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]));
    }

    #[test]
    fn mirrored_colors_match_left_and_right() {
        let top = 0.25; // a square screen
        let middle = top / 2.0;
        // A point and its reflection across the middle of the top.
        let (a, b) = (0.8f32, (2.0 * middle - 0.8f32).rem_euclid(1.0));
        assert!((place(a, Style::Mirror, 2, top, 0.0) - place(b, Style::Mirror, 2, top, 0.0)).abs() < 1e-5);
        assert_eq!(place(middle, Style::Mirror, 2, top, 0.0), 0.0); // the first color at the top
        assert!((place(middle + 0.5, Style::Mirror, 2, top, 0.0) - 0.5).abs() < 1e-5); // the second at the bottom
    }

    #[test]
    fn colors_survive_the_trip_through_hex() {
        assert_eq!(hex(rgb(0x12AB7F)), 0x12AB7F);
    }

    #[test]
    fn the_edge_is_cut_into_bands_on_the_right_sides() {
        let (w, h, t) = (100.0, 50.0, 4.0);
        let top = band(10.0, 20.0, w, h, t);
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].x, top[0].y, top[0].w, top[0].h), (10.0, 0.0, 10.0, 4.0));
        // Across the top right corner: the end of the top, the start of the right side.
        let corner = band(95.0, 110.0, w, h, t);
        assert_eq!(corner.len(), 2);
        assert_eq!((corner[1].x, corner[1].y, corner[1].h), (96.0, 0.0, 10.0));
        // The bottom runs right to left, the left side bottom to top.
        let bottom = band(150.0, 160.0, w, h, t);
        assert_eq!((bottom[0].x, bottom[0].y), (90.0, 46.0));
        let left = band(250.0, 260.0, w, h, t);
        assert_eq!((left[0].x, left[0].y), (0.0, 40.0));
        // All the way round covers every side once.
        assert_eq!(band(0.0, 300.0, w, h, t).len(), 4);
    }

    #[test]
    fn the_window_lets_clicks_through() {
        let glow = Glow::new();
        assert!(!glow.hwnd.is_invalid(), "{:?}", windows::core::Error::from_thread());
        let style = unsafe { GetWindowLongW(glow.hwnd, GWL_EXSTYLE) } as u32;
        assert_ne!(style & WS_EX_TRANSPARENT.0, 0);
    }

    #[test]
    fn loud_bass_counts_more() {
        let mut bass = [0.0; crate::audio::BARS];
        bass[..3].copy_from_slice(&[1.0, 1.0, 1.0]);
        let mut treble = [0.0; crate::audio::BARS];
        treble[6..].copy_from_slice(&[1.0, 1.0, 1.0]);
        assert!(loudness(&bass) > loudness(&treble) * 2.0);
    }
}
