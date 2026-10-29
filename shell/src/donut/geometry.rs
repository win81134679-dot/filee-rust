//! Pure donut math: slice angles, hit testing, slice outlines and window placement. No Win32 here, so it is unit
//! tested. Ported from upstream's `DonutGeometry.cs`; the numbers must match (spec 01 §4).
//!
//! Angles are clockwise from 12 o'clock, in radians; slice i is centred on i·span so the first one sits at the top.

use std::f64::consts::{PI, TAU};

/// How far a hovered slice pops out, and the hit-test slack that goes with it.
pub const POP_OUT: f64 = 7.0;
/// Constant gap between neighbouring slices.
pub const GAP: f64 = 3.0;
/// A group's ring: gap to the donut and its width.
pub const SUB_GAP: f64 = 6.0;
pub const SUB_BAND: f64 = 46.0;
/// Room kept around the donut for a group's ring (gap + band + margin).
pub const SUB_RING_SPACE: f64 = SUB_GAP + SUB_BAND + 10.0;
/// How far past an open ring the pointer still counts as on its way to it, and the backdrop's margin.
pub const BACKDROP_MARGIN: f64 = 8.0;
/// A "leave" this far inside the donut's edge went through a see-through seam, not out of the window.
pub const LEAVE_EDGE: f64 = 12.0;
pub const SUB_OVERHANG: f64 = 0.25;
pub const MIN_SUB_WIDTH: f64 = 44.0;
/// Rounding below this (px) is invisible: the slice stays sharp.
pub const MIN_CORNER: f64 = 0.25;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

impl Pt {
    pub const fn new(x: f64, y: f64) -> Pt {
        Pt { x, y }
    }
    pub fn dist(self, o: Pt) -> f64 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

/// What is under a point of the donut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Outside,
    /// The hole (cancel / close).
    Center,
    Segment(usize),
    /// The ring of a donut without slices.
    EmptyRing,
    /// A slice (second) of the ring the group slice (first) fans out.
    SubSegment(usize, usize),
}

pub fn span(count: usize) -> f64 {
    if count == 0 {
        TAU
    } else {
        TAU / count as f64
    }
}

pub fn mid_angle(index: usize, count: usize) -> f64 {
    index as f64 * span(count)
}

pub fn point_at(center: Pt, radius: f64, angle: f64) -> Pt {
    Pt::new(center.x + radius * angle.sin(), center.y - radius * angle.cos())
}

pub fn angle_of(center: Pt, p: Pt) -> f64 {
    let a = (p.x - center.x).atan2(center.y - p.y);
    if a < 0.0 {
        a + TAU
    } else {
        a
    }
}

/// `Math.IEEERemainder(x, 2π)`: x wrapped to -π..π.
pub fn wrap(x: f64) -> f64 {
    x - TAU * (x / TAU).round()
}

/// Index of the slice holding `angle` (centre-aligned: slice 0 covers [-span/2, +span/2)).
pub fn index_at(angle: f64, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let s = span(count);
    Some((((angle + s / 2.0) / s).floor() as i64).rem_euclid(count as i64) as usize)
}

/// Width of one slice of a group's ring: the fan covers the group's slice and a quarter of each neighbour, but never
/// gets narrower than `MIN_SUB_WIDTH` along its middle nor wider than one full turn between the slices.
pub fn sub_span(parent_span: f64, count: usize, mid_radius: f64) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let mut s = parent_span * (1.0 + 2.0 * SUB_OVERHANG) / count as f64;
    if mid_radius > 0.0 {
        s = s.max(MIN_SUB_WIDTH / mid_radius);
    }
    s.min(TAU / count as f64)
}

pub fn sub_mid_angle(parent_mid: f64, sub: usize, count: usize, sub_span: f64) -> f64 {
    parent_mid + (sub as f64 - (count as f64 - 1.0) / 2.0) * sub_span
}

/// Which slice of a group's fan (between the radii) holds `p`.
pub fn sub_index_at(
    center: Pt,
    p: Pt,
    inner: f64,
    outer: f64,
    parent_mid: f64,
    count: usize,
    sub_span: f64,
) -> Option<usize> {
    let d = p.dist(center);
    if count == 0 || d < inner || d > outer {
        return None;
    }
    let delta = wrap(angle_of(center, p) - parent_mid);
    let sub = (delta / sub_span + count as f64 / 2.0).floor();
    (sub >= 0.0 && (sub as usize) < count).then_some(sub as usize)
}

/// The sharp-sector hit test (no gaps, no rounded corners). `slack` extends the outer radius for popped-out slices.
pub fn hit_test(center: Pt, p: Pt, inner: f64, outer: f64, count: usize, slack: f64) -> Hit {
    let d = p.dist(center);
    if d < inner {
        Hit::Center
    } else if d > outer + slack {
        Hit::Outside
    } else if count == 0 {
        Hit::EmptyRing
    } else {
        Hit::Segment(index_at(angle_of(center, p), count).unwrap_or(0))
    }
}

/// Side of the (square) window in DIP: the donut, its pop-out, a margin and, if any face has a group, room for a ring.
pub fn side_for(outer_radius: f64, reserve_sub_ring: bool) -> f64 {
    2.0 * (outer_radius + POP_OUT + 14.0 + if reserve_sub_ring { SUB_RING_SPACE } else { 0.0 })
}

/// Top-left of a `side_px` square centred on the cursor, clamped into the work area `(l, t, r, b)`.
pub fn window_position(cursor: (i32, i32), side_px: i32, area: (i32, i32, i32, i32)) -> (i32, i32) {
    let x = (cursor.0 - side_px / 2).clamp(area.0, area.0.max(area.2 - side_px));
    let y = (cursor.1 - side_px / 2).clamp(area.1, area.1.max(area.3 - side_px));
    (x, y)
}

/// The area that must stay at least alpha 1/255 so Windows delivers drags and clicks to the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Backing {
    /// The whole window (drag mode while it waits for files).
    Full,
    /// The donut disc, plus the fan of a group that is open or about to open.
    Disc { radius: f64, fan: Option<Fan> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fan {
    pub mid: f64,
    pub half_span: f64,
    pub inner: f64,
    pub reach: f64,
}

impl Backing {
    pub fn contains(&self, center: Pt, p: Pt) -> bool {
        match self {
            Backing::Full => true,
            Backing::Disc { radius, fan } => {
                let d = p.dist(center);
                if d <= *radius {
                    return true;
                }
                fan.as_ref().is_some_and(|f| {
                    d >= f.inner && d <= f.reach && wrap(angle_of(center, p) - f.mid).abs() <= f.half_span
                })
            }
        }
    }
}

/// One step of a slice outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Line(Pt),
    Arc { to: Pt, radius: f64, large: bool, clockwise: bool },
}

/// A closed slice outline starting at `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct Wedge {
    pub start: Pt,
    pub steps: Vec<Step>,
    /// The rounding used after clamping (0: sharp).
    pub corner: f64,
}

/// Outline of a slice of `span` radians centred on `mid` between the radii, with a constant `gap` to its neighbours
/// and corners rounded by `corner_radius` (shrunk to what fits; 0 gives the sharp slice).
pub fn wedge(center: Pt, inner: f64, outer: f64, mid: f64, span: f64, gap: f64, corner_radius: f64) -> Wedge {
    let outer_half = (span / 2.0 - gap / 2.0 / outer).max(0.001);
    let inner_half = (span / 2.0 - gap / 2.0 / inner).max(0.001);
    let radius = corner_radius.min((outer - inner) / 2.0);
    if radius > MIN_CORNER {
        if let Some(w) = rounded(center, inner, outer, mid, outer_half, inner_half, radius) {
            return w;
        }
        // Too thin or short for it: the largest rounding that fits, by halving (so a growing slice rounds smoothly).
        let (mut best, mut low, mut high) = (None, 0.0, radius);
        for _ in 0..14 {
            let r = (low + high) / 2.0;
            match rounded(center, inner, outer, mid, outer_half, inner_half, r) {
                Some(w) => {
                    best = Some(w);
                    low = r;
                }
                None => high = r,
            }
        }
        if let Some(w) = best {
            if low > MIN_CORNER {
                return w;
            }
        }
    }
    Wedge {
        start: point_at(center, outer, mid - outer_half),
        steps: vec![
            Step::Arc {
                to: point_at(center, outer, mid + outer_half),
                radius: outer,
                large: outer_half * 2.0 > PI,
                clockwise: true,
            },
            Step::Line(point_at(center, inner, mid + inner_half)),
            Step::Arc {
                to: point_at(center, inner, mid - inner_half),
                radius: inner,
                large: inner_half * 2.0 > PI,
                clockwise: false,
            },
        ],
        corner: 0.0,
    }
}

fn rounded(center: Pt, r1: f64, r2: f64, mid: f64, outer_half: f64, inner_half: f64, radius: f64) -> Option<Wedge> {
    // The slice is symmetric about its middle: work out the corners on its start side and mirror them.
    let outer_side = point_at(center, r2, mid - outer_half);
    let inner_side = point_at(center, r1, mid - inner_half);
    let o = corner(center, outer_side, inner_side, r2, radius, true)?;
    let i = corner(center, outer_side, inner_side, r1, radius, false)?;
    // The two corners on a side must not overlap, nor reach past the middle into the other side's.
    if o.along >= i.along || offset(center, o.center, mid) >= 0.0 || offset(center, i.center, mid) >= 0.0 {
        return None;
    }
    let mirror = |p: Pt| point_at(center, p.dist(center), 2.0 * mid - angle_of(center, p));
    let outer_arc = -offset(center, o.center, mid);
    let inner_arc = -offset(center, i.center, mid);
    let small = |to: Pt| Step::Arc { to, radius, large: false, clockwise: true };
    Some(Wedge {
        start: o.on_arc,
        steps: vec![
            Step::Arc { to: mirror(o.on_arc), radius: r2, large: outer_arc > PI / 2.0, clockwise: true },
            small(mirror(o.on_line)),
            Step::Line(mirror(i.on_line)),
            small(mirror(i.on_arc)),
            Step::Arc { to: i.on_arc, radius: r1, large: inner_arc > PI / 2.0, clockwise: false },
            small(i.on_line),
            Step::Line(o.on_line),
            small(o.on_arc),
        ],
        corner: radius,
    })
}

struct Corner {
    center: Pt,
    on_arc: Pt,
    on_line: Pt,
    along: f64,
}

/// The rounded corner where the start side (outer_side -> inner_side) meets the circle of radius `circle`: the arc's
/// centre, where it touches the circle and the side, and how far along the side it touches it.
fn corner(center: Pt, outer_side: Pt, inner_side: Pt, circle: f64, radius: f64, outer: bool) -> Option<Corner> {
    let length = outer_side.dist(inner_side);
    if length <= 0.0 {
        return None;
    }
    let u = ((inner_side.x - outer_side.x) / length, (inner_side.y - outer_side.y) / length);
    // Into the slice: the start side's normal on the clockwise (increasing angle) side.
    let a = angle_of(center, outer_side);
    let tangent = (a.cos(), a.sin());
    let mut n = (-u.1, u.0);
    if n.0 * tangent.0 + n.1 * tangent.1 < 0.0 {
        n = (-n.0, -n.1);
    }
    // The arc's centre is `radius` inside the side and touches the circle: inside it for the outer edge, outside the
    // hole for the inner one. The nearer of the two crossings is on this side of the middle.
    let distance = if outer { circle - radius } else { circle + radius };
    let w = (outer_side.x + n.0 * radius - center.x, outer_side.y + n.1 * radius - center.y);
    let b = w.0 * u.0 + w.1 * u.1;
    let disc = b * b - (w.0 * w.0 + w.1 * w.1 - distance * distance);
    if disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    if t < 0.0 || t > length {
        return None;
    }
    let c = Pt::new(outer_side.x + n.0 * radius + u.0 * t, outer_side.y + n.1 * radius + u.1 * t);
    let to_c = (c.x - center.x, c.y - center.y);
    let len = (to_c.0 * to_c.0 + to_c.1 * to_c.1).sqrt();
    Some(Corner {
        center: c,
        on_arc: Pt::new(center.x + to_c.0 / len * circle, center.y + to_c.1 / len * circle),
        on_line: Pt::new(outer_side.x + u.0 * t, outer_side.y + u.1 * t),
        along: t,
    })
}

fn offset(center: Pt, p: Pt, mid: f64) -> f64 {
    wrap(angle_of(center, p) - mid)
}

/// 2D affine transform in the row-vector convention of Direct2D / Avalonia: `a.then(b)` applies `a` first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m11: f64,
    pub m12: f64,
    pub m21: f64,
    pub m22: f64,
    pub dx: f64,
    pub dy: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, dx: 0.0, dy: 0.0 };

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine { dx: x, dy: y, ..Affine::IDENTITY }
    }
    pub fn scale(x: f64, y: f64) -> Affine {
        Affine { m11: x, m22: y, ..Affine::IDENTITY }
    }
    pub fn rotate(a: f64) -> Affine {
        Affine { m11: a.cos(), m12: a.sin(), m21: -a.sin(), m22: a.cos(), dx: 0.0, dy: 0.0 }
    }
    pub fn then(self, o: Affine) -> Affine {
        Affine {
            m11: self.m11 * o.m11 + self.m12 * o.m21,
            m12: self.m11 * o.m12 + self.m12 * o.m22,
            m21: self.m21 * o.m11 + self.m22 * o.m21,
            m22: self.m21 * o.m12 + self.m22 * o.m22,
            dx: self.dx * o.m11 + self.dy * o.m21 + o.dx,
            dy: self.dx * o.m12 + self.dy * o.m22 + o.dy,
        }
    }
    #[cfg(test)]
    pub fn apply(self, p: Pt) -> Pt {
        Pt::new(p.x * self.m11 + p.y * self.m21 + self.dx, p.x * self.m12 + p.y * self.m22 + self.dy)
    }

    /// A slice's opening transform: scales up from the centre with a small twist, popped out along its spoke by
    /// `hover` (spec §4.4).
    pub fn slice(center: Pt, mid: f64, open: f64, hover: f64) -> Affine {
        let scale = 0.55 + 0.45 * open;
        let rotate = (1.0 - open.min(1.0)) * -0.35;
        let off = point_at(Pt::new(0.0, 0.0), hover * POP_OUT, mid);
        let s = scale * (1.0 + hover * 0.025);
        Affine::translate(-center.x, -center.y)
            .then(Affine::rotate(rotate))
            .then(Affine::scale(s, s))
            .then(Affine::translate(center.x + off.x, center.y + off.y))
    }

    /// Squeezes a slice about its own spoke to `width` (1: untouched), the flip's card-turn (spec §4.7).
    pub fn flip(center: Pt, mid: f64, width: f64) -> Affine {
        Affine::translate(-center.x, -center.y)
            .then(Affine::rotate(-mid))
            .then(Affine::scale(width, 1.0))
            .then(Affine::rotate(mid))
            .then(Affine::translate(center.x, center.y))
    }
}

/// Overshooting ease used when the slices open.
pub fn ease_out_back(t: f64) -> f64 {
    const C1: f64 = 1.55;
    const C3: f64 = C1 + 1.0;
    1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
}

pub fn ease_in_cubic(t: f64) -> f64 {
    t * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: Pt = Pt::new(100.0, 100.0);

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn spans_and_mid_angles() {
        assert!(close(span(4), PI / 2.0));
        assert!(close(span(0), TAU));
        assert!(close(mid_angle(0, 6), 0.0));
        assert!(close(mid_angle(3, 6), PI));
    }

    #[test]
    fn point_and_angle_round_trip_clockwise_from_top() {
        let top = point_at(C, 50.0, 0.0);
        assert!(close(top.x, 100.0) && close(top.y, 50.0));
        let right = point_at(C, 50.0, PI / 2.0);
        assert!(close(right.x, 150.0) && close(right.y, 100.0));
        for a in [0.0, 0.7, 2.0, 3.5, 5.9] {
            assert!(close(angle_of(C, point_at(C, 30.0, a)), a));
        }
    }

    #[test]
    fn index_is_centre_aligned() {
        // First slice covers [-span/2, +span/2): just left of 12 o'clock is still slice 0.
        assert_eq!(index_at(TAU - 0.01, 4), Some(0));
        assert_eq!(index_at(0.01, 4), Some(0));
        assert_eq!(index_at(PI / 4.0 + 0.01, 4), Some(1));
        assert_eq!(index_at(PI, 4), Some(2));
        assert_eq!(index_at(1.0, 0), None);
    }

    #[test]
    fn hit_test_classifies_hole_ring_and_outside() {
        let (inner, outer) = (63.0, 150.0);
        assert_eq!(hit_test(C, Pt::new(100.0, 110.0), inner, outer, 6, POP_OUT), Hit::Center);
        assert_eq!(hit_test(C, Pt::new(100.0, 100.0 - 100.0), inner, outer, 6, POP_OUT), Hit::Segment(0));
        // Within the pop-out slack counts, past it does not.
        assert_eq!(hit_test(C, Pt::new(100.0, 100.0 - 156.0), inner, outer, 6, POP_OUT), Hit::Segment(0));
        assert_eq!(hit_test(C, Pt::new(100.0, 100.0 - 158.0), inner, outer, 6, POP_OUT), Hit::Outside);
        assert_eq!(hit_test(C, Pt::new(200.0, 100.0), inner, outer, 0, POP_OUT), Hit::EmptyRing);
        // Right of centre of a 4-slice donut is slice 1.
        assert_eq!(hit_test(C, Pt::new(200.0, 100.0), inner, outer, 4, POP_OUT), Hit::Segment(1));
    }

    #[test]
    fn side_matches_the_spec_for_the_default_radius() {
        assert!(close(side_for(150.0, false), 342.0));
        assert!(close(side_for(150.0, true), 466.0));
    }

    #[test]
    fn window_is_centred_on_the_cursor_and_clamped_to_the_work_area() {
        let area = (0, 0, 1920, 1040);
        assert_eq!(window_position((960, 500), 342, area), (789, 329));
        assert_eq!(window_position((10, 10), 342, area), (0, 0));
        assert_eq!(window_position((1919, 1039), 342, area), (1578, 698));
        // Smaller area than the window: stays at the corner.
        assert_eq!(window_position((50, 50), 342, (0, 0, 300, 300)), (0, 0));
        // A second monitor to the left.
        assert_eq!(window_position((-1000, 300), 342, (-1920, 0, 0, 1040)), (-1171, 129));
    }

    #[test]
    fn sub_span_covers_parent_and_a_quarter_of_each_neighbour() {
        let parent = span(6);
        assert!(close(sub_span(parent, 3, 0.0), parent * 1.5 / 3.0));
        // A crowded ring widens to keep slices 44 px wide at the middle radius.
        assert!(close(sub_span(0.1, 6, 100.0), 0.44));
        // But never more than one turn between them.
        assert!(close(sub_span(6.0, 2, 0.0), PI));
        assert!(close(sub_span(1.0, 0, 50.0), 0.0));
    }

    #[test]
    fn sub_slices_are_centred_on_the_group_and_hit_tested_by_angle() {
        let (parent_mid, count, s) = (0.0, 3, 0.4);
        assert!(close(sub_mid_angle(parent_mid, 1, count, s), 0.0));
        assert!(close(sub_mid_angle(parent_mid, 0, count, s), -0.4));
        let (inner, outer) = (163.0, 209.0);
        for k in 0..count {
            let p = point_at(C, 180.0, sub_mid_angle(parent_mid, k, count, s));
            assert_eq!(sub_index_at(C, p, inner, outer, parent_mid, count, s), Some(k));
        }
        // Wrong distance, or beside the fan.
        assert_eq!(sub_index_at(C, point_at(C, 100.0, 0.0), inner, outer, parent_mid, count, s), None);
        assert_eq!(sub_index_at(C, point_at(C, 180.0, 1.0), inner, outer, parent_mid, count, s), None);
        // A fan across 12 o'clock (parent at the top) reaches both sides of it.
        assert_eq!(sub_index_at(C, point_at(C, 180.0, TAU - 0.5), inner, outer, parent_mid, count, s), Some(0));
    }

    #[test]
    fn sharp_wedge_keeps_a_constant_gap() {
        let w = wedge(C, 63.0, 150.0, 0.0, span(6), GAP, 0.0);
        assert_eq!(w.corner, 0.0);
        assert_eq!(w.steps.len(), 3);
        // The straight sides of neighbouring slices are 3 px apart wherever you measure; check the outer end.
        let outer_half = span(6) / 2.0 - GAP / 2.0 / 150.0;
        let a = point_at(C, 150.0, outer_half);
        let b = point_at(C, 150.0, span(6) - outer_half);
        assert!((a.dist(b) - GAP).abs() < 0.01);
        assert_eq!(w.start, point_at(C, 150.0, -outer_half));
    }

    #[test]
    fn rounded_wedge_uses_the_requested_radius_when_it_fits() {
        let w = wedge(C, 63.0, 150.0, 0.0, span(6), GAP, 8.0);
        assert!(close(w.corner, 8.0));
        assert_eq!(w.steps.len(), 8);
        // The path is closed: it ends where it started.
        match w.steps.last().unwrap() {
            Step::Arc { to, .. } => assert!(to.dist(w.start) < 1e-6),
            s => panic!("last step should be a corner arc, got {s:?}"),
        }
    }

    #[test]
    fn rounding_shrinks_for_thin_slices_and_gives_up_when_nothing_fits() {
        // 12 slices on a small donut: the requested 28 can't fit, but some rounding does.
        let w = wedge(C, 30.0, 100.0, 0.0, span(12), GAP, 28.0);
        assert!(w.corner > MIN_CORNER && w.corner < 28.0);
        // Almost no width: sharp.
        let sharp = wedge(C, 30.0, 100.0, 0.0, 0.02, GAP, 28.0);
        assert_eq!(sharp.corner, 0.0);
    }

    #[test]
    fn affine_composition_applies_left_to_right() {
        let t = Affine::translate(-10.0, -10.0).then(Affine::scale(2.0, 2.0)).then(Affine::translate(10.0, 10.0));
        let p = t.apply(Pt::new(20.0, 10.0));
        assert!(close(p.x, 30.0) && close(p.y, 10.0));
        // Clockwise on screen (y down): the top point (0,-1) turns to the right.
        let r = Affine::rotate(PI / 2.0).apply(Pt::new(0.0, -1.0));
        assert!(close(r.x, 1.0) && r.y.abs() < 1e-9);
    }

    #[test]
    fn open_transform_is_identity_when_fully_open_and_not_hovered() {
        let t = Affine::slice(C, 1.0, 1.0, 0.0);
        let p = t.apply(Pt::new(30.0, 40.0));
        assert!(close(p.x, 30.0) && close(p.y, 40.0));
        // Hover pops the slice outward along its spoke (slice 0 is at the top: up, by 7 px).
        let t = Affine::slice(C, 0.0, 1.0, 1.0);
        let p = t.apply(C);
        assert!(close(p.x, 100.0) && close(p.y, 100.0 - POP_OUT));
    }

    #[test]
    fn flip_squeezes_across_the_spoke_and_leaves_the_axis_alone() {
        // Slice at 3 o'clock: its spoke is horizontal, so the squeeze is vertical.
        let t = Affine::flip(C, PI / 2.0, 0.5);
        let on_spoke = t.apply(Pt::new(150.0, 100.0));
        assert!(close(on_spoke.x, 150.0) && close(on_spoke.y, 100.0));
        let beside = t.apply(Pt::new(150.0, 120.0));
        assert!(close(beside.x, 150.0) && close(beside.y, 110.0));
    }

    #[test]
    fn ease_out_back_overshoots_and_ends_at_one() {
        assert!(close(ease_out_back(0.0), 1.0 - 2.55 + 1.55));
        assert!(close(ease_out_back(1.0), 1.0));
        assert!((0..100).any(|i| ease_out_back(i as f64 / 100.0) > 1.0));
        assert!(close(ease_in_cubic(0.5), 0.125));
    }

    #[test]
    fn backing_covers_the_disc_and_only_the_open_fan() {
        let disc = Backing::Disc { radius: 157.0, fan: None };
        assert!(disc.contains(C, point_at(C, 150.0, 1.0)));
        assert!(!disc.contains(C, point_at(C, 180.0, 0.0)));
        let fan = Fan { mid: 0.0, half_span: 0.5, inner: 150.0, reach: 217.0 };
        let with = Backing::Disc { radius: 157.0, fan: Some(fan) };
        assert!(with.contains(C, point_at(C, 180.0, 0.3)));
        assert!(!with.contains(C, point_at(C, 180.0, 0.7)));
        assert!(!with.contains(C, point_at(C, 230.0, 0.0)));
        assert!(Backing::Full.contains(C, Pt::new(-5.0, 900.0)));
    }
}
