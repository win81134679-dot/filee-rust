//! Drawing the donut with Direct2D + DirectWrite into a 32-bit premultiplied DIB (spec 01 §4). The window then hands
//! the DIB to `UpdateLayeredWindow`. Everything is in DIP: the render target's DPI does the scaling.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;

use windows::core::PCWSTR;
use windows_numerics::{Matrix3x2, Vector2};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;

use super::geometry::*;
use super::logic::{Hub, Menu};
use crate::model::{Palette, Slice};

const LABEL_SIZE: f32 = 13.0;
const MIN_LABEL_SIZE: f32 = 10.0;
const FLIP_ARROW: f64 = 9.0;
const MOUSE_W: f64 = 7.0;
const ICON_GAP: f64 = 4.0;
const MOUSE_GAP: f64 = 6.0;

fn v(x: f64, y: f64) -> Vector2 {
    Vector2 { X: x as f32, Y: y as f32 }
}

fn vp(p: Pt) -> Vector2 {
    v(p.x, p.y)
}

/// ARGB (0xAARRGGBB) with its alpha scaled by `mul`.
fn color(argb: u32, mul: f64) -> D2D1_COLOR_F {
    let ch = |shift: u32| ((argb >> shift) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: ch(16), g: ch(8), b: ch(0), a: (ch(24) as f64 * mul).clamp(0.0, 1.0) as f32 }
}

fn matrix(a: Affine) -> Matrix3x2 {
    Matrix3x2 { M11: a.m11 as f32, M12: a.m12 as f32, M21: a.m21 as f32, M22: a.m22 as f32, M31: a.dx as f32, M32: a.dy as f32 }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    stroke: ID2D1StrokeStyle,
    family: RefCell<Vec<u16>>,
    formats: RefCell<HashMap<(u32, bool), IDWriteTextFormat>>,
    label_sizes: RefCell<HashMap<(String, u32), f32>>,
}

/// A laid-out piece of text and how big it came out.
struct Text {
    layout: IDWriteTextLayout,
    width: f32,
    height: f32,
}

impl Renderer {
    pub fn new() -> windows::core::Result<Renderer> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&props)?;
            let brush = target.CreateSolidColorBrush(&color(0xFF000000, 1.0), None)?;
            let stroke = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            Ok(Renderer {
                factory,
                dwrite,
                target,
                brush,
                stroke,
                family: RefCell::new(wide("Segoe UI")),
                formats: RefCell::new(HashMap::new()),
                label_sizes: RefCell::new(HashMap::new()),
            })
        }
    }

    /// The first of the palette's font families that is installed (DirectWrite falls back per glyph for the rest).
    pub fn set_fonts(&self, families: &[String]) {
        unsafe {
            let mut coll: Option<IDWriteFontCollection> = None;
            if self.dwrite.GetSystemFontCollection(&mut coll, false).is_err() {
                return;
            }
            let Some(coll) = coll else { return };
            for f in families {
                let w: Vec<u16> = f.encode_utf16().chain(std::iter::once(0)).collect();
                let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                if coll.FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool() {
                    *self.family.borrow_mut() = f.encode_utf16().collect();
                    self.formats.borrow_mut().clear();
                    self.label_sizes.borrow_mut().clear();
                    return;
                }
            }
        }
    }

    fn format(&self, size: f32, bold: bool) -> Option<IDWriteTextFormat> {
        let key = ((size * 10.0).round() as u32, bold);
        if let Some(f) = self.formats.borrow().get(&key) {
            return Some(f.clone());
        }
        let mut family = self.family.borrow().clone();
        family.push(0);
        let fmt = unsafe {
            let fmt = self
                .dwrite
                .CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    None,
                    if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    windows::core::w!("en-us"),
                )
                .ok()?;
            let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            fmt
        };
        self.formats.borrow_mut().insert(key, fmt.clone());
        Some(fmt)
    }

    /// Text centred in a box `max_width` wide, at most `max_lines` lines (the rest ends in an ellipsis).
    fn text(&self, s: &str, size: f32, bold: bool, max_width: f32, max_lines: u32) -> Option<Text> {
        let fmt = self.format(size, bold)?;
        let max_width = max_width.max(10.0);
        let make = |t: &str| -> Option<(IDWriteTextLayout, DWRITE_TEXT_METRICS)> {
            unsafe {
                let layout = self.dwrite.CreateTextLayout(&wide(t), &fmt, max_width, 1000.0).ok()?;
                let mut m = DWRITE_TEXT_METRICS::default();
                layout.GetMetrics(&mut m).ok()?;
                Some((layout, m))
            }
        };
        let (layout, m) = make(s)?;
        if m.lineCount <= max_lines {
            return Some(Text { layout, width: m.width, height: m.height });
        }
        // Too long: the longest prefix that fits, with an ellipsis.
        let chars: Vec<char> = s.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        let mut best = None;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let t: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "\u{2026}";
            match make(&t) {
                Some((l, m)) if m.lineCount <= max_lines => {
                    best = Some(Text { layout: l, width: m.width, height: m.height });
                    lo = mid;
                }
                _ => hi = mid - 1,
            }
        }
        best.or_else(|| Some(Text { layout, width: m.width, height: m.height }))
    }

    /// 13, or smaller (down to 10) when one of the name's words is wider than the slice would break in the middle.
    /// Chinese and Japanese words are left out: those break between any two characters.
    fn label_size(&self, s: &str, line: f32) -> f32 {
        let line = line.round();
        let key = (s.to_string(), line as u32);
        if let Some(&z) = self.label_sizes.borrow().get(&key) {
            return z;
        }
        let widest = s
            .split_whitespace()
            .filter(|w| !w.chars().any(is_han_or_kana))
            .filter_map(|w| self.text(w, LABEL_SIZE, true, 10_000.0, 1).map(|t| t.width))
            .fold(0.0f32, f32::max);
        let size = if widest < line { LABEL_SIZE } else { (LABEL_SIZE * (line - 1.0) / widest).max(MIN_LABEL_SIZE) };
        self.label_sizes.borrow_mut().insert(key, size);
        size
    }

    // ----------------------------------------------------------------- shapes

    fn path(&self, w: &Wedge) -> Option<ID2D1PathGeometry> {
        unsafe {
            let geo = self.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            sink.BeginFigure(vp(w.start), D2D1_FIGURE_BEGIN_FILLED);
            for step in &w.steps {
                match *step {
                    Step::Line(p) => sink.AddLine(vp(p)),
                    Step::Arc { to, radius, large, clockwise } => sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: vp(to),
                        size: D2D_SIZE_F { width: radius as f32, height: radius as f32 },
                        rotationAngle: 0.0,
                        sweepDirection: if clockwise { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    }),
                }
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            Some(geo)
        }
    }

    /// A full ring (a donut of one slice, or none): two circles, the inner one cut out.
    fn ring(&self, c: Pt, r1: f64, r2: f64) -> Option<ID2D1PathGeometry> {
        unsafe {
            let geo = self.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            sink.SetFillMode(D2D1_FILL_MODE_ALTERNATE);
            for r in [r2, r1] {
                sink.BeginFigure(v(c.x + r, c.y), D2D1_FIGURE_BEGIN_FILLED);
                for to in [v(c.x - r, c.y), v(c.x + r, c.y)] {
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: to,
                        size: D2D_SIZE_F { width: r as f32, height: r as f32 },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: D2D1_ARC_SIZE_SMALL,
                    });
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            }
            sink.Close().ok()?;
            Some(geo)
        }
    }

    fn fill(&self, geo: &ID2D1PathGeometry, c: D2D1_COLOR_F) {
        if c.a <= 0.0 {
            return;
        }
        unsafe {
            self.brush.SetColor(&c);
            self.target.FillGeometry(geo, &self.brush, None::<&ID2D1Brush>);
        }
    }

    fn outline(&self, geo: &ID2D1PathGeometry, c: D2D1_COLOR_F, width: f32) {
        if c.a <= 0.0 {
            return;
        }
        unsafe {
            self.brush.SetColor(&c);
            self.target.DrawGeometry(geo, &self.brush, width, None::<&ID2D1StrokeStyle>);
        }
    }

    fn circle(&self, c: Pt, r: f64, fill: Option<D2D1_COLOR_F>, border: Option<D2D1_COLOR_F>) {
        let e = D2D1_ELLIPSE { point: vp(c), radiusX: r as f32, radiusY: r as f32 };
        unsafe {
            if let Some(f) = fill {
                self.brush.SetColor(&f);
                self.target.FillEllipse(&e, &self.brush);
            }
            if let Some(b) = border {
                self.brush.SetColor(&b);
                self.target.DrawEllipse(&e, &self.brush, 1.0, None::<&ID2D1StrokeStyle>);
            }
        }
    }

    // ----------------------------------------------------------------- scene

    /// Draws one frame into `hdc` (a `w` x `h` px bitmap) at `scale` (DPI / 96). Nothing is drawn while a drag donut
    /// waits for its files; the window keeps its alpha-1 backing then.
    pub fn draw(&self, hdc: HDC, w: i32, h: i32, scale: f32, menu: &Menu, palette: &Palette) -> windows::core::Result<()> {
        unsafe {
            let rect = RECT { left: 0, top: 0, right: w, bottom: h };
            self.target.BindDC(hdc, &rect)?;
            self.target.SetDpi(96.0 * scale, 96.0 * scale);
            self.target.BeginDraw();
            self.target.SetTransform(&matrix(Affine::IDENTITY));
            self.target.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            self.target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            if menu.has_files() {
                self.scene(menu, palette);
            }
            self.target.EndDraw(None, None)
        }
    }

    fn scene(&self, m: &Menu, p: &Palette) {
        let center = m.center();
        let (r1, r2) = (m.inner(), m.style.outer);
        let n = m.items.len();
        let opening = if m.open.is_empty() { 1.0 } else { (m.open.iter().sum::<f64>() / m.open.len() as f64).clamp(0.0, 1.0) };
        self.shadow(center, r2, opening);

        if n == 0 {
            if let Some(ring) = self.ring(center, r1, r2) {
                self.fill(&ring, color(p.surface, m.style.opacity));
                self.outline(&ring, color(p.border, 1.0), 1.0);
            }
        }

        for (i, item) in m.items.iter().enumerate() {
            let open = m.open.get(i).copied().unwrap_or(1.0);
            let flip_width = match &m.flip {
                Some(f) => f.new_width(i),
                None => 1.0,
            };
            if open <= 0.001 || flip_width == 0.0 {
                continue;
            }
            let hover = m.hover.get(i).map_or(0.0, |s| s.value);
            let mid = mid_angle(i, n);
            let hot = Some(i) == m.active_index() && item.enabled;
            self.slice(
                p,
                m.style.opacity,
                m.style.corner,
                &SliceDraw { center, r1, r2, mid, span: span(n), count: n, open, hover, flip_width, item, hot },
            );
        }
        if let Some(f) = &m.flip {
            for (i, o) in f.old.iter().enumerate() {
                let width = f.old_width(i);
                if width == 0.0 || o.open <= 0.001 {
                    continue;
                }
                self.slice(
                    p,
                    m.style.opacity,
                    m.style.corner,
                    &SliceDraw {
                        center,
                        r1,
                        r2,
                        mid: o.mid,
                        span: o.span,
                        count: f.old.len(),
                        open: o.open,
                        hover: o.hover,
                        flip_width: width,
                        item: &o.item,
                        hot: o.hot,
                    },
                );
            }
        }

        if let (Some(g), true) = (m.shown_expanded, m.expand.value > 0.01) {
            self.group_ring(m, p, g, opening);
        }
        self.hub(m, p, center, r1);
    }

    /// A soft drop shadow: a radial gradient ring fakes it without blur (cheap on any machine).
    fn shadow(&self, c: Pt, r2: f64, opacity: f64) {
        let radius = r2 + 14.0;
        let stop = |position: f64, a: f32| D2D1_GRADIENT_STOP { position: position as f32, color: D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a } };
        let stops = [stop(0.0, 0.0), stop((r2 - 18.0) / radius, 0.0), stop((r2 - 2.0) / radius, 46.0 / 255.0), stop(1.0, 0.0)];
        unsafe {
            let Ok(coll) = self.target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: v(c.x, c.y + 4.0),
                gradientOriginOffset: v(0.0, 0.0),
                radiusX: radius as f32,
                radiusY: radius as f32,
            };
            let Ok(brush) = self.target.CreateRadialGradientBrush(&props, None, &coll) else { return };
            brush.SetOpacity(opacity.clamp(0.0, 1.0) as f32);
            let e = D2D1_ELLIPSE { point: v(c.x, c.y + 4.0), radiusX: radius as f32, radiusY: radius as f32 };
            self.target.FillEllipse(&e, &brush);
        }
    }

    fn slice(&self, p: &Palette, opacity: f64, corner: f64, s: &SliceDraw) {
        if s.span < 0.01 {
            return;
        }
        let shape = if s.count == 1 || s.span >= std::f64::consts::TAU - 0.001 {
            self.ring(s.center, s.r1, s.r2)
        } else {
            self.path(&wedge(s.center, s.r1, s.r2, s.mid, s.span, GAP, corner))
        };
        let Some(shape) = shape else { return };
        let alpha = s.open.clamp(0.0, 1.0);
        let t = Affine::slice(s.center, s.mid, s.open, s.hover).then(Affine::flip(s.center, s.mid, s.flip_width));
        unsafe {
            self.target.SetTransform(&matrix(t));
        }
        self.fill(&shape, color(p.surface, alpha * opacity * if s.item.enabled { 1.0 } else { 0.55 }));
        self.outline(&shape, color(p.border, alpha), 1.0);
        // The accent fades in and out with the highlight; a disabled slice never gets it.
        if s.hover > 0.01 && s.item.enabled {
            self.fill(&shape, color(p.accent, 0.82 * s.hover.clamp(0.0, 1.0) * alpha));
        }
        let (maxw, maxh) = (s.span * (s.r1 + s.r2) / 2.0 - 10.0, s.r2 - s.r1 - 16.0);
        self.label(p, s.item, s.center, (s.r1 + s.r2) / 2.0, s.mid, maxw, maxh, s.hot, alpha, true);
        unsafe {
            self.target.SetTransform(&matrix(Affine::IDENTITY));
        }
    }

    /// The small ring a group slice fans out, growing out of its slice.
    fn group_ring(&self, m: &Menu, p: &Palette, group: usize, opening: f64) {
        let children = m.children(group);
        if children.is_empty() {
            return;
        }
        let center = m.center();
        let (inner, outer) = m.sub_radii();
        let parent_mid = mid_angle(group, m.items.len());
        let sub_span = m.sub_span_of(children.len());
        let t = m.expand.value.max(0.0);
        let anchor = point_at(center, inner, parent_mid);
        let scale = 0.8 + 0.2 * t;
        let alpha = t.clamp(0.0, 1.0) * opening;
        let tf = Affine::translate(-anchor.x, -anchor.y).then(Affine::scale(scale, scale)).then(Affine::translate(anchor.x, anchor.y));
        unsafe {
            self.target.SetTransform(&matrix(tf));
        }
        for (k, child) in children.iter().enumerate() {
            let mid = sub_mid_angle(parent_mid, k, children.len(), sub_span);
            let Some(shape) = self.path(&wedge(center, inner, outer, mid, sub_span, GAP, m.style.corner)) else { continue };
            self.fill(&shape, color(p.surface, alpha * m.style.opacity * if child.enabled { 1.0 } else { 0.55 }));
            self.outline(&shape, color(p.border, alpha), 1.0);
            let hot = Some(k) == m.sub_hover && m.expanded == Some(group) && child.enabled;
            if hot {
                self.fill(&shape, color(p.accent, 0.82 * alpha));
            }
            let mid_r = (inner + outer) / 2.0;
            self.label(p, child, center, mid_r, mid, sub_span * mid_r - 8.0, SUB_BAND - 8.0, hot, alpha, false);
        }
        unsafe {
            self.target.SetTransform(&matrix(Affine::IDENTITY));
        }
    }

    /// A slice's name (and, under it, its short target text) centred on its middle.
    #[allow(clippy::too_many_arguments)]
    fn label(&self, p: &Palette, item: &Slice, c: Pt, radius: f64, angle: f64, max_w: f64, max_h: f64, hot: bool, alpha: f64, caption: bool) {
        let fg = if hot {
            p.accent_text
        } else if item.enabled {
            p.text
        } else {
            p.text_muted
        };
        let width = max_w.clamp(30.0, 120.0) as f32;
        let name = keep_korean_words(&item.label);
        let size = self.label_size(&name, width);
        let Some(label) = self.text(&name, size, true, width, 2) else { return };
        let show_caption = caption
            && !item.sub_label.is_empty()
            && !item.sub_label.eq_ignore_ascii_case(&item.label)
            && (label.height as f64 + 14.0) < max_h;
        let cap = show_caption
            .then(|| {
                let lines = if (label.height as f64 + 28.0) < max_h { 2 } else { 1 };
                self.text(&item.sub_label, 10.5, false, width, lines)
            })
            .flatten();
        let total = label.height + cap.as_ref().map_or(0.0, |t| t.height);
        let anchor = point_at(c, radius, angle);
        let y = anchor.y as f32 - total / 2.0;
        self.draw_text(&label, anchor.x as f32, y, width, color(fg, alpha));
        if let Some(cap) = &cap {
            self.draw_text(cap, anchor.x as f32, y + label.height, width, color(fg_caption(p, hot), alpha));
        }
    }

    /// Draws text centred on `cx` (the layout's box is `box_w` wide, so the box is what is centred).
    fn draw_text(&self, t: &Text, cx: f32, top: f32, box_w: f32, c: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&c);
            self.target.DrawTextLayout(Vector2 { X: cx - box_w.max(10.0) / 2.0, Y: top }, &t.layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    /// The hole: title, what the pointer is on, and the flip hint.
    fn hub(&self, m: &Menu, p: &Palette, c: Pt, r1: f64) {
        let hub: Hub = m.hub();
        let radius = (r1 - 6.0) * (1.0 + m.center_pulse.value * 0.06);
        self.circle(c, radius, Some(color(p.surface, 1.0)), Some(color(p.border, 1.0)));
        if m.external_center_hot {
            self.circle(c, radius, Some(color(p.danger, 0.18)), None);
        }
        let max_w = (radius * 1.6) as f32;
        let title = self.text(&hub.title, 14.0, true, max_w, 2);
        let sub = (!hub.subtitle.is_empty()).then(|| self.text(&hub.subtitle, 11.5, false, max_w, 2)).flatten();
        let icons = FLIP_ARROW + if hub.hint_wheel { MOUSE_W + MOUSE_GAP } else { 0.0 } + ICON_GAP;
        let hint = (!hub.hint.is_empty())
            .then(|| self.text(&hub.hint, 10.5, false, (max_w as f64 * 0.85 - icons) as f32, 1))
            .flatten();
        let th = title.as_ref().map_or(0.0, |t| t.height);
        let total = th + sub.as_ref().map_or(0.0, |t| t.height + 2.0) + hint.as_ref().map_or(0.0, |t| t.height + 5.0);
        let mut y = c.y as f32 - total / 2.0;
        if let Some(t) = &title {
            self.draw_text(t, c.x as f32, y, max_w, color(p.text, 1.0));
        }
        y += th;
        if let Some(t) = &sub {
            self.draw_text(t, c.x as f32, y + 2.0, max_w, color(if hub.subtitle_danger { p.danger } else { p.text_muted }, 1.0));
            y += t.height + 2.0;
        }
        if let Some(t) = &hint {
            let muted = color(p.text_muted, 0.85);
            let left = c.x - (icons + t.width as f64) / 2.0;
            let middle = y as f64 + 5.0 + t.height as f64 / 2.0;
            let mut x = left;
            if hub.hint_wheel {
                self.mouse(Pt::new(x, middle), muted);
                x += MOUSE_W + MOUSE_GAP;
            }
            self.flip_arrow(Pt::new(x + FLIP_ARROW / 2.0, middle), FLIP_ARROW / 2.0, muted);
            let text_left = left + icons;
            let box_w = (max_w as f64 * 0.85 - icons).max(10.0);
            self.draw_text(t, (text_left + t.width as f64 / 2.0) as f32, y + 5.0, box_w as f32, muted);
        }
    }

    /// A small arrow turning clockwise: most of a circle with its head at the end. Drawn, not typed: some fonts have
    /// no glyph for it and show a box.
    fn flip_arrow(&self, c: Pt, r: f64, col: D2D1_COLOR_F) {
        let (start, sweep) = (-PI / 3.0, PI * 5.0 / 3.0);
        let at = |a: f64| Pt::new(c.x + r * a.cos(), c.y + r * a.sin());
        let end = start + sweep;
        unsafe {
            let Ok(geo) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geo.Open() else { return };
            sink.BeginFigure(vp(at(start)), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: vp(at(end)),
                size: D2D_SIZE_F { width: r as f32, height: r as f32 },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_LARGE,
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            if sink.Close().is_err() {
                return;
            }
            self.brush.SetColor(&col);
            self.target.DrawGeometry(&geo, &self.brush, 1.2, &self.stroke);
            // The head: two strokes back from the tip, along the tangent and either side of it.
            let tip = at(end);
            let along = (-end.sin(), end.cos());
            let outward = (end.cos(), end.sin());
            let head = r * 0.75;
            let back = (tip.x - along.0 * head, tip.y - along.1 * head);
            for s in [0.6, -0.6] {
                let from = Pt::new(back.0 + outward.0 * head * s, back.1 + outward.1 * head * s);
                self.target.DrawLine(vp(from), vp(tip), &self.brush, 1.2, &self.stroke);
            }
        }
    }

    /// A small mouse with its wheel: the hint when only the wheel flips.
    fn mouse(&self, at: Pt, col: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&col);
            let r = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: at.x as f32, top: (at.y - 5.0) as f32, right: (at.x + MOUSE_W) as f32, bottom: (at.y + 5.0) as f32 },
                radiusX: (MOUSE_W / 2.0) as f32,
                radiusY: (MOUSE_W / 2.0) as f32,
            };
            self.target.DrawRoundedRectangle(&r, &self.brush, 1.1, None::<&ID2D1StrokeStyle>);
            let x = at.x + MOUSE_W / 2.0;
            self.target.DrawLine(v(x, at.y - 3.0), v(x, at.y - 0.5), &self.brush, 1.1, None::<&ID2D1StrokeStyle>);
        }
    }
}

struct SliceDraw<'a> {
    center: Pt,
    r1: f64,
    r2: f64,
    mid: f64,
    span: f64,
    count: usize,
    open: f64,
    hover: f64,
    flip_width: f64,
    item: &'a Slice,
    hot: bool,
}

fn fg_caption(p: &Palette, hot: bool) -> u32 {
    if hot {
        p.accent_text
    } else {
        p.text_muted
    }
}

/// Chinese characters, kana and the CJK symbols and full-width forms: a line may break between any two.
fn is_han_or_kana(c: char) -> bool {
    matches!(c, '\u{2E80}'..='\u{312F}' | '\u{3190}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF00}'..='\u{FFEF}')
}

fn is_hangul(c: char) -> bool {
    matches!(c, '\u{AC00}'..='\u{D7A3}')
}

/// Korean wraps at spaces, but the layout also breaks between any two syllables; a word joiner keeps words whole.
fn keep_korean_words(s: &str) -> String {
    if !s.chars().any(is_hangul) {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        out.push(c);
        if is_hangul(c) && chars.get(i + 1).is_some_and(|&n| is_hangul(n)) {
            out.push('\u{2060}');
        }
    }
    out
}

/// A top-down 32-bit premultiplied DIB in a memory DC: where the renderer draws and what the layered window shows.
pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    pub width: i32,
    pub height: i32,
}

impl Surface {
    pub fn new(width: i32, height: i32) -> Option<Surface> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
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
            if bits.is_null() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                return None;
            }
            let previous = SelectObject(dc, bitmap.into());
            Some(Surface { dc, bitmap, previous, bits: bits as *mut u32, width, height })
        }
    }

    #[cfg(test)]
    pub fn pixels(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.width * self.height) as usize) }
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.width * self.height) as usize) }
    }

    /// Windows lets mouse and drags fall through pixels with alpha 0, so every pixel of the `backing` area that the
    /// drawing left empty gets alpha 1 (premultiplied black at 1/255: invisible, but it catches).
    pub fn apply_backing(&mut self, backing: &Backing, center: Pt, scale: f64) {
        let w = self.width as usize;
        match backing {
            Backing::Full => self.pixels_mut().iter_mut().filter(|p| **p == 0).for_each(|p| *p = 0x0100_0000),
            Backing::Disc { .. } => {
                for (i, px) in self.pixels_mut().iter_mut().enumerate() {
                    if *px != 0 {
                        continue;
                    }
                    let at = Pt::new(((i % w) as f64 + 0.5) / scale, ((i / w) as f64 + 0.5) / scale);
                    if backing.contains(center, at) {
                        *px = 0x0100_0000;
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::donut::logic::Mode;
    use crate::model::{Faces, SliceAction};
    use std::path::PathBuf;

    fn slice(id: &str, label: &str, sub: &str, enabled: bool) -> Slice {
        Slice {
            preset_id: id.into(),
            label: label.into(),
            sub_label: sub.into(),
            enabled,
            reason: (!enabled).then(|| "No engine can do this conversion".to_string()),
            action: SliceAction::Cli,
            ..Default::default()
        }
    }

    fn sample_menu(palette: &Palette) -> Menu {
        let mut group = slice("g", "Resize", "Group of 3", true);
        group.is_group = true;
        group.action = SliceAction::Group;
        group.children = vec![slice("r1", "Small", "", true), slice("r2", "Medium", "", true), slice("r3", "Large", "", false)];
        let front = vec![
            slice("to-png", "To PNG", "PNG", true),
            slice("to-jpg", "To JPG", "JPG", true),
            group,
            slice("to-webp", "To WebP", "WEBP", true),
            slice("to-pdf", "\u{8f49}\u{70ba} PDF", "PDF", true),
            slice("crop", "Crop", "", false),
        ];
        let back = vec![slice("x", "Grayscale", "", true), slice("y", "Compress", "", true), slice("z", "Rotate", "", true)];
        let mut m = Menu::new();
        m.style = crate::donut::logic::Style::from_settings(palette, &serde_json::Value::Null);
        m.load(
            Faces { profile_id: "images".into(), profile_label: "Images".into(), front, back },
            vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg"), PathBuf::from("c.jpg")],
            Mode::Click,
        );
        m.play_open();
        m
    }

    fn settle(m: &mut Menu, secs: f64) {
        for _ in 0..(secs * 60.0) as usize {
            m.advance(1.0 / 60.0);
        }
    }

    /// Draws the menu and returns its pixels (premultiplied BGRA, top-down) and size.
    fn paint(r: &Renderer, m: &Menu, palette: &Palette, scale: f32) -> (Vec<u32>, i32, i32) {
        let side = (m.side() * scale as f64).ceil() as i32;
        let mut s = Surface::new(side, side).expect("dib");
        r.draw(s.dc, side, side, scale, m, palette).expect("draw");
        s.apply_backing(&m.backing(), m.center(), scale as f64);
        (s.pixels().to_vec(), side, side)
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    /// A minimal PNG writer (stored deflate blocks): the preview needs no image crate.
    fn write_png(path: &std::path::Path, w: i32, h: i32, rgba: &[u8]) {
        let mut raw = Vec::with_capacity(rgba.len() + h as usize);
        for row in rgba.chunks(w as usize * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, b) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            z.extend_from_slice(&(b.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
            z.extend_from_slice(b);
        }
        let (mut a, mut bsum) = (1u32, 0u32);
        for &x in &raw {
            a = (a + x as u32) % 65521;
            bsum = (bsum + a) % 65521;
        }
        z.extend_from_slice(&((bsum << 16) | a).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        std::fs::write(path, out).expect("write png");
    }

    /// Composites premultiplied BGRA over a flat backdrop (so the shadow and the translucent fill show) into RGBA.
    fn over(pixels: &[u32], bg: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(pixels.len() * 4);
        for &p in pixels {
            let (a, r, g, b) = ((p >> 24) & 255, (p >> 16) & 255, (p >> 8) & 255, p & 255);
            let mix = |c: u32, bgc: u8| (c + (255 - a) * bgc as u32 / 255).min(255) as u8;
            out.extend_from_slice(&[mix(r, bg[0]), mix(g, bg[1]), mix(b, bg[2]), 255]);
        }
        out
    }

    fn preview_dir() -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("donut-preview");
        std::fs::create_dir_all(&dir).expect("preview dir");
        dir
    }

    fn at(m: &Menu, i: usize, radius: f64) -> Pt {
        point_at(m.center(), radius, mid_angle(i, m.items.len()))
    }

    #[test]
    fn renders_the_donut_and_keeps_an_alpha_backing() {
        let palette = Palette::default();
        let r = Renderer::new().expect("renderer");
        r.set_fonts(&palette.fonts);
        let mut m = sample_menu(&palette);
        settle(&mut m, 1.0);
        let p = at(&m, 0, 110.0);
        m.pointer_moved(p);
        settle(&mut m, 1.0);
        let (px, w, h) = paint(&r, &m, &palette, 1.0);
        // A slice is drawn (opaque-ish pixels near its middle), the corner of the window stays empty.
        let c = m.center();
        let at_px = |p: Pt| px[(p.y as usize) * w as usize + p.x as usize];
        assert!(at_px(at(&m, 1, 120.0)) >> 24 > 200, "slice fill is nearly opaque");
        assert_eq!(at_px(Pt::new(2.0, 2.0)), 0, "outside the disc stays see-through");
        // Everywhere inside the disc has at least alpha 1, even the hole's seams and the gaps between slices.
        let disc = m.style.outer + POP_OUT - 1.0;
        for y in 0..h {
            for x in 0..w {
                if Pt::new(x as f64 + 0.5, y as f64 + 0.5).dist(c) <= disc {
                    assert!(px[(y * w + x) as usize] >> 24 >= 1, "hole in the backing at {x},{y}");
                }
            }
        }
        let rgba = over(&px, [0xE8, 0xE8, 0xEC]);
        write_png(&preview_dir().join("donut-front-hover.png"), w, h, &rgba);
    }

    #[test]
    fn waiting_drag_donut_draws_nothing_but_covers_the_whole_window() {
        let palette = Palette::default();
        let r = Renderer::new().expect("renderer");
        let mut m = Menu::new();
        m.reset_for_drag();
        let (px, ..) = paint(&r, &m, &palette, 1.0);
        assert!(px.iter().all(|&p| p == 0x0100_0000), "alpha 1 black everywhere, nothing visible");
    }

    #[test]
    fn group_ring_flip_and_dark_theme_render() {
        let r = Renderer::new().expect("renderer");
        // The group ring open, at 150% DPI.
        let palette = Palette::default();
        let mut m = sample_menu(&palette);
        settle(&mut m, 1.0);
        m.set_external_hit(at(&m, 2, 80.0));
        m.set_external_hit(at(&m, 2, 95.0));
        let (a, b) = m.sub_radii();
        m.set_external_hit(point_at(m.center(), (a + b) / 2.0, mid_angle(2, 6)));
        settle(&mut m, 1.0);
        assert!(m.expanded.is_some());
        let (px, w, h) = paint(&r, &m, &palette, 1.5);
        write_png(&preview_dir().join("donut-group-150dpi.png"), w, h, &over(&px, [0xE8, 0xE8, 0xEC]));
        // The ring area carries pixels beyond the disc.
        let ring_px = {
            let p = point_at(m.center(), (a + b) / 2.0, mid_angle(2, 6));
            px[((p.y * 1.5) as usize) * w as usize + (p.x * 1.5) as usize]
        };
        assert!(ring_px >> 24 > 200);

        // Dark theme, mid-flip.
        let dark = Palette {
            dark: true,
            surface: 0xFF262624,
            border: 0x1CFFFFFF,
            text: 0xFFF1EFE8,
            text_muted: 0xFFAEACA4,
            ..Palette::default()
        };
        let mut m = sample_menu(&dark);
        settle(&mut m, 1.0);
        m.pointer_moved(at(&m, 4, 110.0));
        assert!(m.flip());
        m.advance(0.06);
        assert!(m.flip.is_some());
        let (px, w, h) = paint(&r, &m, &dark, 1.0);
        write_png(&preview_dir().join("donut-dark-midflip.png"), w, h, &over(&px, [0x30, 0x30, 0x38]));
    }

    #[test]
    fn long_labels_shrink_then_end_in_an_ellipsis() {
        let r = Renderer::new().expect("renderer");
        let word = "Internationalization";
        let size = r.label_size(word, 60.0);
        assert!((MIN_LABEL_SIZE..LABEL_SIZE).contains(&size), "size {size}");
        assert_eq!(r.label_size("PNG", 60.0), LABEL_SIZE);
        // Han text is never shrunk: it breaks between characters.
        assert_eq!(r.label_size("\u{8f49}\u{70ba}\u{5716}\u{7247}\u{683c}\u{5f0f}", 40.0), LABEL_SIZE);
        let t = r.text(&"word ".repeat(40), 13.0, true, 90.0, 2).expect("text");
        assert!(t.height < 13.0 * 1.5 * 3.0, "kept to two lines, height {}", t.height);
    }

    #[test]
    fn korean_words_are_kept_whole_and_cjk_detected() {
        assert_eq!(keep_korean_words("ZIP"), "ZIP");
        assert_eq!(keep_korean_words("\u{d558}\u{b098}"), "\u{d558}\u{2060}\u{b098}");
        assert!(is_han_or_kana('\u{8f49}') && is_han_or_kana('\u{3042}') && !is_han_or_kana('a'));
    }
}
