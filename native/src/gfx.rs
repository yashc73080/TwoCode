//! Drawing with the graphics card: Direct2D into a transparent composition surface. Nothing runs between frames.
use std::collections::HashMap;
use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::SHCreateMemStream;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub fn colour(hex: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 255) as f32 / 255.0,
        g: ((hex >> 8) & 255) as f32 / 255.0,
        b: (hex & 255) as f32 / 255.0,
        a: alpha,
    }
}

pub struct Gfx {
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    swap: IDXGISwapChain1,
    _target: ID2D1Bitmap1,
    _dcomp: IDCompositionDevice,
    _comp_target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    formats: HashMap<(u32, bool, Align, bool), IDWriteTextFormat>,
    art: Option<(u64, ID2D1BitmapBrush1)>,
    art_size: (f32, f32),
    art_colour: Option<u32>,
    pub scale: f32,
}

impl Gfx {
    pub fn new(hwnd: HWND, width_px: u32, height_px: u32, scale: f32) -> Result<Gfx> {
        unsafe {
            let mut device = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
            let device: ID3D11Device = device.expect("a Direct3D device");
            let dxgi: IDXGIDevice = device.cast()?;
            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width_px,
                Height: height_px,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap = dxgi_factory.CreateSwapChainForComposition(&dxgi, &desc, None)?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let dc = d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let surface: IDXGISurface = swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let target = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            dc.SetTarget(&target);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let comp_target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            comp_target.SetRoot(&visual)?;
            dcomp.Commit()?;
            let brush = dc.CreateSolidColorBrush(&colour(0, 1.0), None)?;
            let dwrite = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Gfx {
                factory,
                dc,
                swap,
                _target: target,
                _dcomp: dcomp,
                _comp_target: comp_target,
                _visual: visual,
                brush,
                dwrite,
                formats: HashMap::new(),
                art: None,
                art_size: (1.0, 1.0),
                art_colour: None,
                scale,
            })
        }
    }

    pub fn begin(&self) {
        unsafe {
            self.dc.BeginDraw();
            self.dc.SetTransform(&matrix(self.scale, 0.0, 0.0));
            self.dc.Clear(Some(&colour(0, 0.0)));
        }
    }

    /// Hands the frame to Windows and waits for the screen, so an animation runs at the display's own speed.
    pub fn end(&self) -> Result<()> {
        unsafe {
            self.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()
        }
    }

    pub fn clip(&self, r: Rect) {
        unsafe { self.dc.PushAxisAlignedClip(&d2d_rect(r), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) }
    }

    pub fn unclip(&self) {
        unsafe { self.dc.PopAxisAlignedClip() }
    }

    pub fn fill(&self, r: Rect, radius: f32, hex: u32, alpha: f32) {
        unsafe {
            self.brush.SetColor(&colour(hex, alpha));
            let rounded = D2D1_ROUNDED_RECT { rect: d2d_rect(r), radiusX: radius, radiusY: radius };
            self.dc.FillRoundedRectangle(&rounded, &self.brush);
        }
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, hex: u32, alpha: f32) {
        self.fill(Rect { x: x - radius, y: y - radius, w: radius * 2.0, h: radius * 2.0 }, radius, hex, alpha);
    }

    pub fn polygon(&self, points: &[(f32, f32)], hex: u32, alpha: f32) {
        let build = || -> Result<ID2D1PathGeometry1> {
            unsafe {
                let path = self.factory.CreatePathGeometry()?;
                let sink = path.Open()?;
                sink.BeginFigure(point(points[0]), D2D1_FIGURE_BEGIN_FILLED);
                for p in &points[1..] {
                    sink.AddLine(point(*p));
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
                Ok(path)
            }
        };
        if let Ok(path) = build() {
            unsafe {
                self.brush.SetColor(&colour(hex, alpha));
                self.dc.FillGeometry(&path, &self.brush, None);
            }
        }
    }

    pub fn text(&mut self, text: &str, r: Rect, size: f32, bold: bool, align: Align, hex: u32, alpha: f32) {
        self.draw_text(text, r, size, bold, align, false, hex, alpha);
    }

    pub fn text_wrapped(&mut self, text: &str, r: Rect, size: f32, bold: bool, align: Align, hex: u32, alpha: f32) {
        self.draw_text(text, r, size, bold, align, true, hex, alpha);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(&mut self, text: &str, r: Rect, size: f32, bold: bool, align: Align, wrap: bool, hex: u32, alpha: f32) {
        let Some(format) = self.format(size, bold, align, wrap) else { return };
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.brush.SetColor(&colour(hex, alpha));
            self.dc.DrawText(&wide, &format, &d2d_rect(r), &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// How wide this text is on one line, trailing spaces included.
    pub fn text_width(&mut self, text: &str, size: f32, bold: bool) -> f32 {
        let Some(format) = self.format(size, bold, Align::Left, false) else { return 0.0 };
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide, &format, 10_000.0, 100.0) else { return 0.0 };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.widthIncludingTrailingWhitespace
        }
    }

    fn format(&mut self, size: f32, bold: bool, align: Align, wrap: bool) -> Option<IDWriteTextFormat> {
        let key = ((size * 10.0) as u32, bold, align, wrap);
        if let Some(found) = self.formats.get(&key) {
            return Some(found.clone());
        }
        let make = || -> Result<IDWriteTextFormat> {
            unsafe {
                let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
                let font: PCWSTR = w!("Segoe UI");
                let format = self.dwrite.CreateTextFormat(font, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!(""))?;
                format.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                format.SetTextAlignment(match align {
                    Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                    Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                    Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
                })?;
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
                format.SetTrimming(&trimming, &sign)?;
                Ok(format)
            }
        };
        let format = make().ok()?;
        self.formats.insert(key, format.clone());
        Some(format)
    }

    /// Decodes album art once per track; the encoded bytes are not kept.
    pub fn set_art(&mut self, rev: u64, bytes: Option<&[u8]>) {
        if self.art.as_ref().is_some_and(|(have, _)| *have == rev) {
            return;
        }
        let brush = bytes.and_then(|b| self.decode(b).ok());
        if brush.is_none() {
            self.art_colour = None;
        }
        self.art = brush.map(|b| (rev, b));
    }

    pub fn has_art(&self) -> bool {
        self.art.is_some()
    }

    fn decode(&mut self, bytes: &[u8]) -> Result<ID2D1BitmapBrush1> {
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = SHCreateMemStream(Some(bytes)).ok_or_else(windows::core::Error::empty)?;
            let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
            let frame = decoder.GetFrame(0)?;
            let scaler = wic.CreateBitmapScaler()?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h)?;
            // Crop nothing, but keep the bitmap small: the cover is drawn at under 100 pixels.
            let side = 160u32;
            let fit = side as f32 / w.min(h).max(1) as f32;
            let (out_w, out_h) = (((w as f32 * fit) as u32).max(1), ((h as f32 * fit) as u32).max(1));
            scaler.Initialize(&frame, out_w, out_h, WICBitmapInterpolationModeFant)?;
            let converter = wic.CreateFormatConverter()?;
            converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            let bitmap = self.dc.CreateBitmapFromWicBitmap(&converter, None)?;
            self.art_size = (out_w as f32, out_h as f32);
            // One pixel of the cover is its average colour, used for the glow behind the music page.
            let tiny = wic.CreateBitmapScaler()?;
            tiny.Initialize(&frame, 1, 1, WICBitmapInterpolationModeFant)?;
            let tiny_rgb = wic.CreateFormatConverter()?;
            tiny_rgb.Initialize(&tiny, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            let mut pixel = [0u8; 4];
            tiny_rgb.CopyPixels(std::ptr::null(), 4, &mut pixel)?;
            self.art_colour = Some(((pixel[2] as u32) << 16) | ((pixel[1] as u32) << 8) | pixel[0] as u32);
            let bitmap: ID2D1Bitmap = bitmap.cast()?;
            self.dc.CreateBitmapBrush(&bitmap, None, None)
        }
    }

    /// The cover's colour rising from the bottom edge, black at the top where the screen ends.
    pub fn tint(&self, r: Rect, radius: f32, alpha: f32) {
        let Some(hex) = self.art_colour else { return };
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.25, color: colour(hex, 0.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: colour(hex, 0.5 * alpha) },
        ];
        unsafe {
            let target: &ID2D1RenderTarget = &self.dc;
            let Ok(collection) = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let line = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: point((r.x, r.y)),
                endPoint: point((r.x, r.y + r.h)),
            };
            let Ok(brush) = target.CreateLinearGradientBrush(&line, None, &collection) else { return };
            let rounded = D2D1_ROUNDED_RECT { rect: d2d_rect(r), radiusX: radius, radiusY: radius };
            self.dc.FillRoundedRectangle(&rounded, &brush);
        }
    }

    pub fn draw_art(&self, r: Rect, radius: f32, alpha: f32) {
        let Some((_, brush)) = &self.art else { return };
        unsafe {
            // Cover the square and centre the image, so wide video thumbnails are not squashed.
            let zoom = (r.w / self.art_size.0).max(r.h / self.art_size.1);
            let x = r.x + (r.w - self.art_size.0 * zoom) / 2.0;
            let y = r.y + (r.h - self.art_size.1 * zoom) / 2.0;
            brush.SetTransform(&matrix(zoom, x, y));
            brush.SetOpacity(alpha);
            let rounded = D2D1_ROUNDED_RECT { rect: d2d_rect(r), radiusX: radius, radiusY: radius };
            self.dc.FillRoundedRectangle(&rounded, brush);
        }
    }
}

fn matrix(scale: f32, x: f32, y: f32) -> windows_numerics::Matrix3x2 {
    windows_numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: x, M32: y }
}

fn point(p: (f32, f32)) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: p.0, Y: p.1 }
}

fn d2d_rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
}
