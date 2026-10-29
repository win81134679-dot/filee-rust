//! Draws toast cards with Direct2D + DirectWrite into a premultiplied BGRA DIB (spec 03 §1.2, §7.1).
//! Coordinates are DIPs; the render target gets one scale transform. `toast.rs` owns the window and calls this.

use std::collections::HashMap;

use windows::core::{Interface, PCWSTR};
use windows_numerics::{Matrix3x2, Vector2};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;

use crate::model::Palette;
use super::theme::{shade, with_alpha};

/// Card width in DIPs (the window is 360 wide with a 12 DIP transparent edge for the shadow).
pub const CARD_W: f32 = 336.0;
pub const EDGE: f32 = 12.0;
pub const WINDOW_W: f32 = 360.0;
pub const CARD_GAP: f32 = 8.0;
const PAD_X: f32 = 14.0;
const PAD_Y: f32 = 12.0;
const ROW_GAP: f32 = 8.0;
const BUTTON_H: f32 = 30.0;
const CLOSE: f32 = 20.0;
const MAX_ERROR_LINES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Close,
    Folder,
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtnKind {
    Close,
    Cancel,
    Retry,
    Open,
    Folder,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BtnSpec {
    pub kind: BtnKind,
    pub label: String,
    pub icon: Option<Icon>,
    pub accent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Progress {
    None,
    /// 0..1
    Fraction(f32),
    /// A moving segment; the value is the phase 0..1.
    Indeterminate(f32),
}

/// Everything one card shows, already translated.
#[derive(Clone, Debug, PartialEq)]
pub struct CardModel {
    pub id: u64,
    pub title: String,
    pub status: String,
    pub progress: Progress,
    pub buttons: Vec<BtnSpec>,
    /// One failure per line; shown in the danger colour, at most four lines.
    pub errors: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    fn d2d(&self, dx: f32, dy: f32) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x + dx, top: self.y + dy, right: self.x + self.w + dx, bottom: self.y + self.h + dy }
    }
}

/// Card-local positions of everything in a card.
#[derive(Clone, Debug, Default)]
pub struct CardLayout {
    pub height: f32,
    title: Rect,
    close: Rect,
    bar: Option<Rect>,
    status: Rect,
    /// (button, its rectangle, width of its label, index in the model's buttons)
    pub buttons: Vec<(BtnKind, Rect, f32, usize)>,
    errors: Option<(Rect, String)>,
}

impl CardLayout {
    /// The button under a card-local point.
    pub fn hit(&self, x: f32, y: f32) -> Option<BtnKind> {
        if self.close.contains(x, y) {
            return Some(BtnKind::Close);
        }
        self.buttons.iter().find(|(_, r, _, _)| r.contains(x, y)).map(|(k, _, _, _)| *k)
    }
}

/// Which button is hovered / pressed on the card being drawn.
#[derive(Clone, Copy, Debug, Default)]
pub struct Interaction {
    pub hover: Option<BtnKind>,
    pub pressed: Option<BtnKind>,
}

// ---- Direct2D / DirectWrite resources ------------------------------------------------------------------

pub struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory2,
    fonts: Vec<String>,
    locale: Vec<u16>,
    fallback: Option<IDWriteFontFallback>,
    formats: HashMap<(u32, bool), IDWriteTextFormat>,
    paths: HashMap<Icon, ID2D1PathGeometry>,
}

impl Gfx {
    pub fn new(fonts: &[String], language: &str) -> windows::core::Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut gfx = Gfx {
                d2d,
                dw,
                fonts: Vec::new(),
                locale: wide(language),
                fallback: None,
                formats: HashMap::new(),
                paths: HashMap::new(),
            };
            gfx.set_fonts(fonts, language);
            for icon in [Icon::Close, Icon::Folder, Icon::Refresh] {
                if let Some(geometry) = gfx.build_path(icon_data(icon)) {
                    gfx.paths.insert(icon, geometry);
                }
            }
            Ok(gfx)
        }
    }

    /// Font families in fallback order (first one draws, the rest cover missing glyphs).
    pub fn set_fonts(&mut self, fonts: &[String], language: &str) {
        self.fonts = if fonts.is_empty() { vec!["Segoe UI".to_string()] } else { fonts.to_vec() };
        self.locale = wide(language);
        self.formats.clear();
        self.fallback = self.build_fallback();
    }

    fn build_fallback(&self) -> Option<IDWriteFontFallback> {
        unsafe {
            let builder = self.dw.CreateFontFallbackBuilder().ok()?;
            let names: Vec<Vec<u16>> = self.fonts.iter().map(|f| wide(f)).collect();
            let pointers: Vec<*const u16> = names.iter().map(|n| n.as_ptr()).collect();
            let everything = [DWRITE_UNICODE_RANGE { first: 0, last: 0x10FFFF }];
            builder
                .AddMapping(&everything, &pointers, None::<&IDWriteFontCollection>, PCWSTR::null(), PCWSTR::null(), 1.0)
                .ok()?;
            let system = self.dw.GetSystemFontFallback().ok()?;
            builder.AddMappings(&system).ok()?;
            builder.CreateFontFallback().ok()
        }
    }

    fn format(&mut self, size: f32, bold: bool) -> Option<IDWriteTextFormat> {
        let key = ((size * 10.0) as u32, bold);
        if let Some(f) = self.formats.get(&key) {
            return Some(f.clone());
        }
        let family = wide(&self.fonts[0]);
        let format = unsafe {
            self.dw
                .CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    None::<&IDWriteFontCollection>,
                    if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(self.locale.as_ptr()),
                )
                .ok()?
        };
        if let Some(fallback) = &self.fallback {
            if let Ok(f1) = format.cast::<IDWriteTextFormat1>() {
                unsafe {
                    let _ = f1.SetFontFallback(fallback);
                }
            }
        }
        self.formats.insert(key, format.clone());
        Some(format)
    }

    /// A text layout of `text`. `max_w` wraps (or, with `single_line`, trims with an ellipsis).
    fn layout(&mut self, text: &str, size: f32, bold: bool, max_w: f32, single_line: bool) -> Option<IDWriteTextLayout> {
        self.layout_utf16(&wide_no_nul(text), size, bold, max_w, single_line)
    }

    fn layout_utf16(&mut self, text: &[u16], size: f32, bold: bool, max_w: f32, single_line: bool) -> Option<IDWriteTextLayout> {
        let format = self.format(size, bold)?;
        unsafe {
            let layout = self.dw.CreateTextLayout(text, &format, max_w.max(1.0), 10_000.0).ok()?;
            if single_line {
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&format) {
                    let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                    let _ = layout.SetTrimming(&trimming, &sign);
                }
            }
            Some(layout)
        }
    }

    fn metrics(layout: &IDWriteTextLayout) -> DWRITE_TEXT_METRICS {
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = layout.GetMetrics(&mut m);
        }
        m
    }

    /// Width of `text` on one line.
    fn text_width(&mut self, text: &str, size: f32, bold: bool) -> f32 {
        self.layout(text, size, bold, 10_000.0, false).map(|l| Self::metrics(&l).widthIncludingTrailingWhitespace).unwrap_or(0.0)
    }

    fn line_height(&mut self, size: f32, bold: bool) -> f32 {
        self.layout("Ag", size, bold, 10_000.0, false).map(|l| Self::metrics(&l).height).unwrap_or(size * 1.4)
    }

    /// Wrapped text cut to `max_lines` (last visible line ends in an ellipsis). Returns the layout and its height.
    fn wrapped(&mut self, text: &str, size: f32, width: f32, max_lines: usize) -> Option<(IDWriteTextLayout, f32)> {
        let units = wide_no_nul(text);
        let layout = self.layout_utf16(&units, size, false, width, false)?;
        let m = Self::metrics(&layout);
        if (m.lineCount as usize) <= max_lines {
            return Some((layout, m.height));
        }
        let mut lines = vec![DWRITE_LINE_METRICS::default(); m.lineCount as usize];
        let mut count = 0u32;
        unsafe {
            layout.GetLineMetrics(Some(&mut lines), &mut count).ok()?;
        }
        let keep: usize = lines.iter().take(max_lines).map(|l| l.length as usize).sum();
        let mut cut = units[..keep.min(units.len())].to_vec();
        while matches!(cut.last(), Some(0x0A | 0x0D | 0x20)) {
            cut.pop();
        }
        cut.pop(); // make room for the ellipsis
        cut.push(0x2026);
        let layout = self.layout_utf16(&cut, size, false, width, false)?;
        let height = Self::metrics(&layout).height;
        Some((layout, height))
    }

    fn build_path(&self, data: &str) -> Option<ID2D1PathGeometry> {
        let segments = parse_path(data)?;
        unsafe {
            let geometry = self.d2d.CreatePathGeometry().ok()?;
            let sink = geometry.Open().ok()?;
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            let mut open = false;
            for seg in segments {
                match seg {
                    Seg::M(x, y) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        sink.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_FILLED);
                        open = true;
                    }
                    Seg::L(x, y) => sink.AddLine(Vector2 { X: x, Y: y }),
                    Seg::C(a, b, c, d, e, f) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: Vector2 { X: a, Y: b },
                        point2: Vector2 { X: c, Y: d },
                        point3: Vector2 { X: e, Y: f },
                    }),
                    Seg::A { rx, ry, rot, large, sweep, x, y } => sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: Vector2 { X: x, Y: y },
                        size: D2D_SIZE_F { width: rx, height: ry },
                        rotationAngle: rot,
                        sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    }),
                    Seg::Z => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                            open = false;
                        }
                    }
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close().ok()?;
            Some(geometry)
        }
    }

    // ---- layout ------------------------------------------------------------------------------------------

    /// Positions everything in `card` (card-local DIPs) and returns its height.
    pub fn measure(&mut self, card: &CardModel) -> CardLayout {
        let content_w = CARD_W - 2.0 * PAD_X;
        let mut layout = CardLayout::default();
        let mut y = PAD_Y;

        let title_w = content_w - CLOSE - ROW_GAP;
        let title_h = self.layout(&card.title, 14.0, true, title_w, true).map(|l| Self::metrics(&l).height).unwrap_or(19.0);
        let row_h = title_h.max(CLOSE);
        layout.title = Rect::new(PAD_X, y + (row_h - title_h) / 2.0, title_w, title_h);
        layout.close = Rect::new(PAD_X + content_w - CLOSE, y, CLOSE, CLOSE);
        y += row_h + ROW_GAP;

        if !matches!(card.progress, Progress::None) {
            layout.bar = Some(Rect::new(PAD_X, y, content_w, 6.0));
            y += 6.0 + ROW_GAP;
        }

        if !card.status.is_empty() || !card.buttons.is_empty() {
            let status_need = if card.status.is_empty() { 0.0 } else { self.text_width(&card.status, 12.0, false) + ROW_GAP };
            let widths: Vec<(f32, f32)> = card
                .buttons
                .iter()
                .map(|spec| {
                    let label_w = self.text_width(&spec.label, 14.0, false);
                    let icon_w = if spec.icon.is_some() { 13.0 + 6.0 } else { 0.0 };
                    (label_w, label_w + icon_w + 20.0 + 2.0)
                })
                .collect();
            // Keep every button, but give the status text room first: "Open" is the first to go.
            let mut keep: Vec<usize> = (0..card.buttons.len()).collect();
            let used = |keep: &[usize]| keep.iter().map(|i| widths[*i].1 + 6.0).sum::<f32>();
            while content_w - used(&keep) < status_need {
                match keep.iter().position(|i| card.buttons[*i].kind == BtnKind::Open) {
                    Some(at) => {
                        keep.remove(at);
                    }
                    None => break,
                }
            }
            let mut x_end = PAD_X + content_w;
            for i in keep.iter().rev() {
                let (label_w, w) = widths[*i];
                layout.buttons.push((card.buttons[*i].kind, Rect::new(x_end - w, y, w, BUTTON_H), label_w, *i));
                x_end -= w + 6.0;
            }
            layout.buttons.reverse();
            let status_w = (x_end + 6.0 - PAD_X - ROW_GAP).max(0.0);
            let line_h = self.line_height(12.0, false);
            layout.status = Rect::new(PAD_X, y + (BUTTON_H - line_h) / 2.0, status_w, line_h);
            y += BUTTON_H;
        }

        if !card.errors.is_empty() {
            if let Some((_, h)) = self.wrapped(&card.errors, 12.0, content_w, MAX_ERROR_LINES) {
                y += ROW_GAP;
                layout.errors = Some((Rect::new(PAD_X, y, content_w, h), card.errors.clone()));
                y += h;
            }
        }
        layout.height = y + PAD_Y;
        layout
    }
}

// ---- drawing -------------------------------------------------------------------------------------------

/// A DC render target with one reusable brush.
pub struct Canvas {
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
}

impl Canvas {
    pub fn new(gfx: &Gfx) -> windows::core::Result<Canvas> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = gfx.d2d.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&rgba(0xFF000000, 1.0), None)?;
            Ok(Canvas { rt, brush })
        }
    }

    /// Binds `surface`, clears it to transparent and applies the DIP -> pixel scale.
    pub fn begin(&self, surface: &Surface, scale: f32) -> windows::core::Result<()> {
        unsafe {
            let rect = RECT { left: 0, top: 0, right: surface.w, bottom: surface.h };
            self.rt.BindDC(surface.dc, &rect)?;
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            self.rt.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        }
        Ok(())
    }

    pub fn end(&self) -> windows::core::Result<()> {
        unsafe { self.rt.EndDraw(None, None) }
    }

    fn fill(&self, color: u32, mul: f32, rect: D2D_RECT_F, radius: f32) {
        unsafe {
            self.brush.SetColor(&rgba(color, mul));
            self.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    fn stroke(&self, color: u32, mul: f32, rect: D2D_RECT_F, radius: f32) {
        let inset = D2D_RECT_F { left: rect.left + 0.5, top: rect.top + 0.5, right: rect.right - 0.5, bottom: rect.bottom - 0.5 };
        unsafe {
            self.brush.SetColor(&rgba(color, mul));
            let rounded = D2D1_ROUNDED_RECT { rect: inset, radiusX: (radius - 0.5).max(0.0), radiusY: (radius - 0.5).max(0.0) };
            self.rt.DrawRoundedRectangle(&rounded, &self.brush, 1.0, None::<&ID2D1StrokeStyle>);
        }
    }

    fn text(&self, layout: &IDWriteTextLayout, x: f32, y: f32, color: u32, mul: f32) {
        unsafe {
            self.brush.SetColor(&rgba(color, mul));
            self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    /// Fills `icon` (a 24x24 path) into a `size` square at (x, y).
    fn icon(&self, gfx: &Gfx, icon: Icon, x: f32, y: f32, size: f32, scale: f32, color: u32, mul: f32) {
        let Some(path) = gfx.paths.get(&icon) else { return };
        let k = size / 24.0 * scale;
        unsafe {
            self.rt.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: x * scale, M32: y * scale });
            self.brush.SetColor(&rgba(color, mul));
            self.rt.FillGeometry(path, &self.brush, None::<&ID2D1Brush>);
            self.rt.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        }
    }

    /// Draws one card with its top-left at (ox, oy) DIPs. `mul` fades the whole card.
    #[allow(clippy::too_many_arguments)]
    pub fn card(
        &self,
        gfx: &mut Gfx,
        pal: &Palette,
        model: &CardModel,
        layout: &CardLayout,
        (ox, oy): (f32, f32),
        scale: f32,
        mul: f32,
        ui: Interaction,
    ) {
        let radius = pal.corner_radius;
        let small = (radius * 0.55).round();
        let rect = D2D_RECT_F { left: ox, top: oy, right: ox + CARD_W, bottom: oy + layout.height };

        // Shadow: two soft layers (spec §4.3 CardShadow), approximated by stacked translucent rounded rects.
        let layers: [(f32, f32, u8); 2] = if pal.dark { [(1.0, 3.0, 0x40), (8.0, 24.0, 0x33)] } else { [(1.0, 3.0, 0x12), (6.0, 18.0, 0x0D)] };
        for (dy, blur, a) in layers {
            let steps = blur.ceil().max(2.0) as i32;
            for k in 0..steps {
                let grow = -blur / 2.0 + (k as f32 + 0.5) * blur / steps as f32;
                let r = D2D_RECT_F { left: rect.left - grow, top: rect.top + dy - grow, right: rect.right + grow, bottom: rect.bottom + dy + grow };
                self.fill(with_alpha(0, 0xFF), mul * f32::from(a) / 255.0 / steps as f32, r, (radius + grow).max(0.0));
            }
        }
        self.fill(pal.surface, mul, rect, radius);
        self.stroke(pal.border, mul, rect, radius);

        // Title.
        if let Some(l) = gfx.layout(&model.title, 14.0, true, layout.title.w, true) {
            self.text(&l, ox + layout.title.x, oy + layout.title.y, pal.text, mul);
        }

        // Close button.
        let close = layout.close;
        if ui.hover == Some(BtnKind::Close) || ui.pressed == Some(BtnKind::Close) {
            self.fill(pal.surface_alt, mul, close.d2d(ox, oy), small.max(4.0));
        }
        self.icon(gfx, Icon::Close, ox + close.x + 4.0, oy + close.y + 4.0, 12.0, scale, pal.text, mul);

        // Progress bar.
        if let Some(bar) = layout.bar {
            self.fill(pal.surface_alt, mul, bar.d2d(ox, oy), 3.0);
            match model.progress {
                Progress::Fraction(f) => {
                    let f = f.clamp(0.0, 1.0);
                    if f > 0.0 {
                        let w = (bar.w * f).max(bar.h);
                        self.fill(pal.accent, mul, Rect::new(bar.x, bar.y, w, bar.h).d2d(ox, oy), 3.0);
                    }
                }
                Progress::Indeterminate(phase) => {
                    let seg = bar.w * 0.3;
                    let x = bar.x - seg + (bar.w + seg) * phase.rem_euclid(1.0);
                    let (x0, x1) = (x.max(bar.x), (x + seg).min(bar.x + bar.w));
                    if x1 > x0 {
                        self.fill(pal.accent, mul, Rect::new(x0, bar.y, x1 - x0, bar.h).d2d(ox, oy), 3.0);
                    }
                }
                Progress::None => {}
            }
        }

        // Status text.
        if !model.status.is_empty() {
            if let Some(l) = gfx.layout(&model.status, 12.0, false, layout.status.w, true) {
                self.text(&l, ox + layout.status.x, oy + layout.status.y, pal.text_muted, mul);
            }
        }

        // Buttons.
        for (kind, r, label_w, index) in &layout.buttons {
            let spec = &model.buttons[*index];
            let hovered = ui.hover == Some(*kind);
            let pressed = ui.pressed == Some(*kind) && hovered;
            let area = r.d2d(ox, oy);
            let text_color;
            if spec.accent {
                let step = if pal.dark { 1.0 } else { -1.0 };
                let fill = if pressed {
                    shade(pal.accent, 0.30 * step)
                } else if hovered {
                    shade(pal.accent, 0.15 * step)
                } else {
                    pal.accent
                };
                self.fill(fill, mul, area, small);
                text_color = pal.accent_text;
            } else {
                let fill = if pressed {
                    if pal.dark { 0xFF3A3A36 } else { 0xFFE6E3DA }
                } else if hovered {
                    pal.surface_alt
                } else {
                    pal.surface
                };
                let border = match (pal.dark, hovered) {
                    (false, false) => 0x29000000,
                    (false, true) => 0x3D000000,
                    (true, false) => 0x38FFFFFF,
                    (true, true) => 0x4DFFFFFF,
                };
                self.fill(fill, mul, area, small);
                self.stroke(border, mul, area, small);
                text_color = pal.text;
            }
            let icon_w = if spec.icon.is_some() { 13.0 + 6.0 } else { 0.0 };
            let mut x = r.x + (r.w - (icon_w + label_w)) / 2.0;
            if let Some(icon) = spec.icon {
                self.icon(gfx, icon, ox + x, oy + r.y + (BUTTON_H - 13.0) / 2.0, 13.0, scale, text_color, mul);
                x += icon_w;
            }
            if let Some(l) = gfx.layout(&spec.label, 14.0, false, label_w + 4.0, true) {
                let h = Gfx::metrics(&l).height;
                self.text(&l, ox + x, oy + r.y + (BUTTON_H - h) / 2.0, text_color, mul);
            }
        }

        // Failure lines.
        if let Some((r, text)) = &layout.errors {
            if let Some((l, _)) = gfx.wrapped(text, 12.0, r.w, MAX_ERROR_LINES) {
                self.text(&l, ox + r.x, oy + r.y, pal.danger, mul);
            }
        }
    }
}

// ---- the DIB the window shows ------------------------------------------------------------------------

/// A top-down 32 bpp DIB selected into a memory DC; Direct2D draws premultiplied BGRA into it.
pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    #[allow(dead_code)]
    bits: *mut u8,
    pub w: i32,
    pub h: i32,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Surface> {
        if w <= 0 || h <= 0 {
            return None;
        }
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let previous = SelectObject(dc, bitmap.into());
            Some(Surface { dc, bitmap, previous, bits: bits as *mut u8, w, h })
        }
    }

    /// The premultiplied BGRA pixels, row after row (the preview tests read them).
    #[allow(dead_code)]
    pub fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

// ---- small helpers -----------------------------------------------------------------------------------

fn rgba(c: u32, mul: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((c >> 16) & 0xFF) as f32 / 255.0,
        g: ((c >> 8) & 0xFF) as f32 / 255.0,
        b: (c & 0xFF) as f32 / 255.0,
        a: ((c >> 24) & 0xFF) as f32 / 255.0 * mul.clamp(0.0, 1.0),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_no_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Material Design Icons paths (Apache-2.0), 24x24, from upstream's Icons.axaml.
fn icon_data(icon: Icon) -> &'static str {
    match icon {
        Icon::Close => "M19,6.41L17.59,5L12,10.59L6.41,5L5,6.41L10.59,12L5,17.59L6.41,19L12,13.41L17.59,19L19,17.59L13.41,12L19,6.41Z",
        Icon::Folder => "M10,4H4C2.89,4 2,4.89 2,6V18A2,2 0 0,0 4,20H20A2,2 0 0,0 22,18V8C22,6.89 21.1,6 20,6H12L10,4Z",
        Icon::Refresh => "M17.65,6.35C16.2,4.9 14.21,4 12,4A8,8 0 0,0 4,12A8,8 0 0,0 12,20C15.73,20 18.84,17.45 19.73,14H17.65C16.83,16.33 14.61,18 12,18A6,6 0 0,1 6,12A6,6 0 0,1 12,6C13.66,6 15.14,6.69 16.22,7.78L13,11H20V4L17.65,6.35Z",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    M(f32, f32),
    L(f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    A { rx: f32, ry: f32, rot: f32, large: bool, sweep: bool, x: f32, y: f32 },
    Z,
}

/// SVG path data limited to absolute M L H V C A Z (all the icons use).
fn parse_path(data: &str) -> Option<Vec<Seg>> {
    let mut tokens: Vec<Result<char, f32>> = Vec::new();
    let chars: Vec<char> = data.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() {
            tokens.push(Ok(c));
            i += 1;
        } else if c == '-' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == '.';
            while i < chars.len() && (chars[i].is_ascii_digit() || (chars[i] == '.' && !dot)) {
                dot |= chars[i] == '.';
                i += 1;
            }
            tokens.push(Err(chars[start..i].iter().collect::<String>().parse().ok()?));
        } else {
            i += 1; // separators
        }
    }
    let mut out = Vec::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let mut p = 0;
    let mut cmd = ' ';
    let take = |p: &mut usize, n: usize| -> Option<Vec<f32>> {
        let mut v = Vec::new();
        for _ in 0..n {
            match tokens.get(*p) {
                Some(Err(x)) => v.push(*x),
                _ => return None,
            }
            *p += 1;
        }
        Some(v)
    };
    while p < tokens.len() {
        if let Ok(c) = tokens[p] {
            cmd = c;
            p += 1;
            if c == 'Z' || c == 'z' {
                out.push(Seg::Z);
                continue;
            }
        } else if cmd == 'M' {
            cmd = 'L'; // extra pairs after a moveto are linetos
        } else if cmd == ' ' || cmd == 'Z' {
            return None;
        }
        match cmd {
            'M' => {
                let v = take(&mut p, 2)?;
                (cx, cy) = (v[0], v[1]);
                out.push(Seg::M(cx, cy));
            }
            'L' => {
                let v = take(&mut p, 2)?;
                (cx, cy) = (v[0], v[1]);
                out.push(Seg::L(cx, cy));
            }
            'H' => {
                cx = take(&mut p, 1)?[0];
                out.push(Seg::L(cx, cy));
            }
            'V' => {
                cy = take(&mut p, 1)?[0];
                out.push(Seg::L(cx, cy));
            }
            'C' => {
                let v = take(&mut p, 6)?;
                (cx, cy) = (v[4], v[5]);
                out.push(Seg::C(v[0], v[1], v[2], v[3], v[4], v[5]));
            }
            'A' => {
                let v = take(&mut p, 7)?;
                (cx, cy) = (v[5], v[6]);
                out.push(Seg::A { rx: v[0], ry: v[1], rot: v[2], large: v[3] != 0.0, sweep: v[4] != 0.0, x: v[5], y: v[6] });
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_paths_parse() {
        for icon in [Icon::Close, Icon::Folder, Icon::Refresh] {
            let segs = parse_path(icon_data(icon)).unwrap_or_else(|| panic!("{icon:?}"));
            assert!(matches!(segs.first(), Some(Seg::M(..))));
            assert_eq!(segs.last(), Some(&Seg::Z));
        }
        let folder = parse_path(icon_data(Icon::Folder)).unwrap();
        assert!(folder.iter().any(|s| matches!(s, Seg::A { sweep: false, .. })));
        // Implicit repetition and H/V.
        let segs = parse_path("M0,0 10,0V5H0Z").unwrap();
        assert_eq!(segs, vec![Seg::M(0.0, 0.0), Seg::L(10.0, 0.0), Seg::L(10.0, 5.0), Seg::L(0.0, 5.0), Seg::Z]);
        assert!(parse_path("M0,0 Q1,1 2,2").is_none());
    }

    #[test]
    fn hit_testing_uses_card_local_rectangles() {
        let layout = CardLayout {
            close: Rect::new(300.0, 10.0, 20.0, 20.0),
            buttons: vec![(BtnKind::Cancel, Rect::new(200.0, 60.0, 60.0, 30.0), 30.0, 0)],
            ..Default::default()
        };
        assert_eq!(layout.hit(305.0, 15.0), Some(BtnKind::Close));
        assert_eq!(layout.hit(210.0, 70.0), Some(BtnKind::Cancel));
        assert_eq!(layout.hit(10.0, 10.0), None);
    }
}
