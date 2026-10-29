//! The donut's state machine: faces, hover, group rings, keyboard, flip and the open/close animation. No Win32: the
//! window feeds it pointer/key events and time, the renderer reads it, and what it wants done (run a preset, close)
//! comes out as [`Effect`]s. Spec: docs/spec/01-trigger-donut.md §3.4, §4.

use std::f64::consts::{PI, TAU};
use std::path::PathBuf;

use super::anim::{Spring, CRITICAL, DAMPING, STIFFNESS};
use super::geometry::*;
use crate::model::{Faces, Palette, Slice};
use crate::ui::i18n;

pub const MAX_SLICES: usize = 12;
pub const MAX_SUB: usize = 6;
const OPEN_DURATION: f64 = 0.42;
const OPEN_STAGGER: f64 = 0.028;
const CLOSE_DURATION: f64 = 0.16;
const CLOSE_STAGGER: f64 = 0.01;
const EXPAND_DELAY: f64 = 0.16;
const OUTWARD_PIXELS: f64 = 10.0;
const KEYBOARD_JITTER: f64 = 6.0;
const SLICE_TURN: f64 = 0.12;
const FLIP_STAGGER: f64 = 0.08;
const WHEEL_QUIET_MS: u64 = 300;
/// Keys that never flip the donut (they do something else in it).
const RESERVED_FLIP_KEYS: [&str; 8] = ["Escape", "Enter", "Space", "Left", "Right", "Up", "Down", "Backspace"];

/// Sizes and behaviour from settings.json's `donut` and the palette.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Outer radius in DIP, 100..240.
    pub outer: f64,
    /// Hole radius as a share of the outer one (clamped again when used: 0.2..0.75).
    pub hole_ratio: f64,
    /// Opacity of the slice fill, 0.1..1.
    pub opacity: f64,
    /// Slice corner radius: half the theme's, 0..14.
    pub corner: f64,
    pub flip_key: Option<String>,
    pub flip_wheel: bool,
    pub motion: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style { outer: 150.0, hole_ratio: 0.42, opacity: 0.96, corner: 8.0, flip_key: Some("Tab".into()), flip_wheel: true, motion: true }
    }
}

impl Style {
    pub fn from_settings(palette: &Palette, donut: &serde_json::Value) -> Style {
        let num = |k: &str, d: f64| donut.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite()).unwrap_or(d);
        let flip_key = match donut.get("flipKey").and_then(|v| v.as_str()) {
            None => Some("Tab".to_string()),
            Some(k) => {
                let k = k.trim();
                if k.is_empty() {
                    None
                } else if RESERVED_FLIP_KEYS.iter().any(|r| r.eq_ignore_ascii_case(k)) {
                    Some("Tab".to_string())
                } else {
                    Some(k.to_string())
                }
            }
        };
        Style {
            outer: num("outerRadius", 150.0).clamp(100.0, 240.0),
            hole_ratio: num("holeRatio", 0.42),
            opacity: num("opacity", 0.96).clamp(0.1, 1.0),
            corner: (palette.corner_radius as f64).clamp(0.0, 28.0) * 0.5,
            flip_key,
            flip_wheel: donut.get("flipWithWheel").and_then(|v| v.as_bool()).unwrap_or(true),
            motion: !palette.reduce_motion,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Files are being dragged in; the window never takes the keyboard.
    Drag,
    /// Opened for known files; has focus, closes when it loses it.
    Click,
}

/// What the donut wants done; the controller drains these after each event.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// A slice (or a slice of a group's ring) was clicked or entered.
    Invoke { index: usize, sub: Option<usize> },
    /// The hole was clicked or Esc pressed on the donut.
    Close,
    /// Right-click on a slice: edit its preset.
    Edit { index: usize, sub: Option<usize> },
    /// The close animation ended: hide the window.
    CloseDone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Space,
    Escape,
    Backspace,
    Other,
}

/// A slice of the face that was turned away from, kept while it folds.
#[derive(Clone, Debug)]
pub struct OldSlice {
    pub item: Slice,
    pub mid: f64,
    pub span: f64,
    pub open: f64,
    pub hover: f64,
    pub hot: bool,
    pub delay: f64,
}

#[derive(Clone, Debug)]
pub struct Flip {
    pub clock: f64,
    pub old: Vec<OldSlice>,
    pub new_delay: Vec<f64>,
}

impl Flip {
    fn duration(&self) -> f64 {
        let longest = self.old.iter().map(|o| o.delay).chain(self.new_delay.iter().copied()).fold(0.0, f64::max);
        longest + SLICE_TURN
    }

    /// Progress 0..1 of the card-turn of a slice with this delay.
    fn progress(&self, delay: f64) -> f64 {
        ((self.clock - delay) / SLICE_TURN).clamp(0.0, 1.0)
    }

    /// Width factor of an old-face slice (0 = not drawn any more).
    pub fn old_width(&self, i: usize) -> f64 {
        let u = self.progress(self.old[i].delay);
        if u >= 0.5 {
            0.0
        } else {
            (PI * u).cos().abs().max(0.01)
        }
    }

    /// Width factor of a new-face slice (0 = not drawn yet).
    pub fn new_width(&self, i: usize) -> f64 {
        let u = self.progress(self.new_delay.get(i).copied().unwrap_or(0.0));
        if u < 0.5 {
            0.0
        } else {
            (PI * u).cos().abs().max(0.01)
        }
    }
}

/// The text of the hub (the hole).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hub {
    pub title: String,
    pub subtitle: String,
    pub subtitle_danger: bool,
    pub hint: String,
    /// A small mouse before the hint: the wheel flips and no key does.
    pub hint_wheel: bool,
}

pub struct Menu {
    pub style: Style,
    pub mode: Mode,
    pub faces: Option<Faces>,
    pub back: bool,
    pub items: Vec<Slice>,
    pub files: Vec<PathBuf>,
    reserve_sub_ring: bool,

    pub hover: Vec<Spring>,
    pub center_pulse: Spring,
    pub expand: Spring,
    pub open: Vec<f64>,
    open_clock: Option<f64>,
    pub closing: bool,

    hover_index: Option<usize>,
    external_highlight: Option<usize>,
    pub external_center_hot: bool,
    pub sub_hover: Option<usize>,
    pub expanded: Option<usize>,
    pub shown_expanded: Option<usize>,
    pending_group: Option<usize>,
    pending_distance: f64,
    pending_elapsed: f64,
    ring_from_keys: bool,
    keyboard_hold: bool,
    key_anchor: Option<Pt>,
    last_pointer: Option<Pt>,
    last_external_at: Option<Pt>,
    press: Option<Option<usize>>,

    pub flip: Option<Flip>,
    flip_queued: bool,
    last_wheel_ms: Option<u64>,
    effects: Vec<Effect>,
}

impl Default for Menu {
    fn default() -> Self {
        Menu::new()
    }
}

impl Menu {
    pub fn new() -> Menu {
        Menu {
            style: Style::default(),
            mode: Mode::Drag,
            faces: None,
            back: false,
            items: Vec::new(),
            files: Vec::new(),
            reserve_sub_ring: true,
            hover: Vec::new(),
            center_pulse: Spring::default(),
            expand: Spring::default(),
            open: Vec::new(),
            open_clock: None,
            closing: false,
            hover_index: None,
            external_highlight: None,
            external_center_hot: false,
            sub_hover: None,
            expanded: None,
            shown_expanded: None,
            pending_group: None,
            pending_distance: 0.0,
            pending_elapsed: 0.0,
            ring_from_keys: false,
            keyboard_hold: false,
            key_anchor: None,
            last_pointer: None,
            last_external_at: None,
            press: None,
            flip: None,
            flip_queued: false,
            last_wheel_ms: None,
            effects: Vec::new(),
        }
    }

    // ---------------------------------------------------------------- layout

    pub fn side(&self) -> f64 {
        side_for(self.style.outer, self.reserve_sub_ring)
    }

    pub fn center(&self) -> Pt {
        Pt::new(self.side() / 2.0, self.side() / 2.0)
    }

    pub fn inner(&self) -> f64 {
        self.style.outer * self.style.hole_ratio.clamp(0.2, 0.75)
    }

    pub fn sub_radii(&self) -> (f64, f64) {
        let inner = self.style.outer + POP_OUT + SUB_GAP;
        (inner, inner + SUB_BAND)
    }

    pub fn sub_span_of(&self, count: usize) -> f64 {
        let (a, b) = self.sub_radii();
        sub_span(span(self.items.len()), count, (a + b) / 2.0)
    }

    pub fn children(&self, group: usize) -> &[Slice] {
        self.items.get(group).map_or(&[], |s| s.children.as_slice())
    }

    fn can_expand(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(|s| s.is_group && !s.children.is_empty())
    }

    fn in_fan(&self, p: Pt, group: usize) -> bool {
        let count = self.children(group).len();
        count > 0
            && sub_index_at(self.center(), p, 0.0, f64::MAX, mid_angle(group, self.items.len()), count, self.sub_span_of(count))
                .is_some()
    }

    fn on_way_to_ring(&self, p: Pt) -> bool {
        self.expanded.is_some_and(|g| {
            p.dist(self.center()) > (self.inner() + self.style.outer) / 2.0 && self.in_fan(p, g)
        })
    }

    pub fn expanded_children(&self) -> Option<&[Slice]> {
        self.expanded.map(|g| self.children(g)).filter(|c| !c.is_empty())
    }

    // ----------------------------------------------------------------- faces

    /// Waiting for files (drag mode before DragEnter): nothing drawn, whole window kept hit-testable.
    pub fn reset_for_drag(&mut self) {
        self.faces = None;
        self.back = false;
        self.files.clear();
        self.mode = Mode::Drag;
        self.reserve_sub_ring = true;
        self.set_items(Vec::new());
    }

    pub fn load(&mut self, faces: Faces, files: Vec<PathBuf>, mode: Mode) {
        let faces = Faces { front: sanitize(faces.front), back: sanitize(faces.back), ..faces };
        if mode == Mode::Click {
            self.reserve_sub_ring = faces.front.iter().chain(&faces.back).any(|s| s.is_group);
        }
        self.mode = mode;
        self.back = false;
        self.files = files;
        let front = faces.front.clone();
        self.faces = Some(faces);
        self.set_items(front);
    }

    fn set_items(&mut self, items: Vec<Slice>) {
        let n = items.len();
        self.items = items;
        self.hover = vec![Spring::default(); n];
        self.open = vec![1.0; n];
        self.hover_index = None;
        self.external_highlight = None;
        self.sub_hover = None;
        self.expanded = None;
        self.shown_expanded = None;
        self.pending_group = None;
        self.expand = Spring::default();
        self.ring_from_keys = false;
        self.keyboard_hold = false;
        self.key_anchor = None;
        self.last_pointer = None;
        self.last_external_at = None;
        self.press = None;
        // A new set of slices ends whatever was animating; a flip starts only after its own swap.
        self.closing = false;
        self.open_clock = None;
        self.flip = None;
        self.flip_queued = false;
        self.effects.clear();
    }

    pub fn last_external_at(&self) -> Option<Pt> {
        self.last_external_at
    }

    pub fn has_files(&self) -> bool {
        self.faces.is_some()
    }

    pub fn other_face(&self) -> &[Slice] {
        match &self.faces {
            Some(f) if self.back => &f.front,
            Some(f) => &f.back,
            None => &[],
        }
    }

    /// The other face has something to flip to: at least one slice that is enabled.
    pub fn can_flip(&self) -> bool {
        self.other_face().iter().any(|s| s.enabled)
    }

    /// The preset a hit would run: an enabled slice that isn't a group, or an enabled slice of a group's ring.
    pub fn preset_at(&self, index: usize, sub: Option<usize>) -> Option<&Slice> {
        let item = self.items.get(index)?;
        let s = match sub {
            Some(k) => item.children.get(k)?,
            None if item.is_group => return None,
            None => item,
        };
        s.enabled.then_some(s)
    }

    pub fn converts(&self, hit: Hit) -> bool {
        match hit {
            Hit::Segment(i) => self.preset_at(i, None).is_some(),
            Hit::SubSegment(i, k) => self.preset_at(i, Some(k)).is_some(),
            _ => false,
        }
    }

    // ------------------------------------------------------------- animation

    pub fn play_open(&mut self) {
        self.closing = false;
        if self.style.motion {
            self.open = vec![0.0; self.items.len()];
            self.open_clock = Some(0.0);
        } else {
            self.open = vec![1.0; self.items.len()];
            self.open_clock = None;
        }
    }

    pub fn play_close(&mut self) {
        self.finish_flip();
        if self.items.is_empty() || !self.style.motion {
            self.effects.push(Effect::CloseDone);
            return;
        }
        self.closing = true;
        self.open_clock = Some(0.0);
    }

    /// Advances every animation by `dt` seconds; true while more frames are needed.
    pub fn advance(&mut self, dt: f64) -> bool {
        let mut active = false;
        let target = self.active_index();
        if !self.style.motion {
            for (i, h) in self.hover.iter_mut().enumerate() {
                *h = Spring::at(if Some(i) == target { 1.0 } else { 0.0 });
            }
            let o = if self.closing { 0.0 } else { 1.0 };
            self.open.iter_mut().for_each(|v| *v = o);
            self.center_pulse = Spring::default();
            self.expand = Spring::at(if self.expanded.is_some() { 1.0 } else { 0.0 });
            if self.expanded.is_none() {
                self.shown_expanded = None;
            }
            self.finish_flip();
            self.open_clock = None;
            if self.closing {
                self.finish_close();
            }
        } else {
            // Popping out bounces; letting go settles without one (an undershoot would flash the accent).
            for (i, h) in self.hover.iter_mut().enumerate() {
                active |= if Some(i) == target {
                    h.step(1.0, dt, STIFFNESS, DAMPING)
                } else {
                    h.step(0.0, dt, STIFFNESS, CRITICAL)
                };
            }
            active |= self.center_pulse.step(0.0, dt, 260.0, 12.0);
            active |= if self.expanded.is_some() {
                self.expand.step(1.0, dt, STIFFNESS, 24.0)
            } else {
                self.expand.step(0.0, dt, STIFFNESS, CRITICAL)
            };
            if self.expanded.is_none() && self.expand.value <= 0.001 {
                self.shown_expanded = None;
            }
            active |= self.step_flip(dt);
            if let Some(clock) = self.open_clock {
                let clock = clock + dt;
                self.open_clock = Some(clock);
                let (stagger, dur) = if self.closing { (CLOSE_STAGGER, CLOSE_DURATION) } else { (OPEN_STAGGER, OPEN_DURATION) };
                let mut done = true;
                for (i, o) in self.open.iter_mut().enumerate() {
                    let t = ((clock - i as f64 * stagger) / dur).clamp(0.0, 1.0);
                    *o = if self.closing { 1.0 - ease_in_cubic(t) } else { ease_out_back(t) };
                    done &= t >= 1.0;
                }
                if done {
                    self.open_clock = None;
                    if self.closing {
                        self.finish_close();
                    }
                } else {
                    active = true;
                }
            }
        }
        if let Some(g) = self.pending_group {
            // The group's slice has been rested on for a while: fan it out.
            self.pending_elapsed += dt;
            if self.pending_elapsed >= EXPAND_DELAY {
                self.expand_group(g);
            }
            active = true;
        }
        active
    }

    fn finish_close(&mut self) {
        self.closing = false;
        self.effects.push(Effect::CloseDone);
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    // ----------------------------------------------------------------- input

    pub fn active_index(&self) -> Option<usize> {
        self.external_highlight.or(self.hover_index)
    }

    pub fn hit_test(&self, p: Pt) -> Hit {
        let center = self.center();
        if let Some(g) = self.expanded {
            let count = self.children(g).len();
            if count > 0 {
                let (a, b) = self.sub_radii();
                if let Some(k) = sub_index_at(center, p, a, b, mid_angle(g, self.items.len()), count, self.sub_span_of(count)) {
                    return Hit::SubSegment(g, k);
                }
            }
        }
        let base = hit_test(center, p, self.inner(), self.style.outer, self.items.len(), POP_OUT);
        // Between the donut and the open ring, in the group's direction: still the group (don't fold on the way).
        if let (Hit::Outside, Some(g)) = (base, self.expanded) {
            if p.dist(center) <= self.sub_radii().0 && self.in_fan(p, g) {
                return Hit::Segment(g);
            }
        }
        base
    }

    fn set_hover(&mut self, i: Option<usize>) {
        self.hover_index = i;
    }

    fn set_sub(&mut self, sub: Option<usize>) {
        self.sub_hover = sub;
    }

    fn clear_pending(&mut self) {
        self.pending_group = None;
        self.pending_elapsed = 0.0;
    }

    fn expand_group(&mut self, group: usize) {
        if group >= self.items.len() {
            return;
        }
        self.ring_from_keys = false;
        // Another ring still folding away: this one grows out of its own slice, not from half open.
        if self.shown_expanded != Some(group) {
            self.expand = Spring::default();
        }
        self.expanded = Some(group);
        self.shown_expanded = Some(group);
        self.clear_pending();
    }

    pub fn collapse_group(&mut self) {
        self.expanded = None;
        self.ring_from_keys = false;
        self.clear_pending();
        self.set_sub(None);
    }

    /// Fans a group out once the pointer means it: after resting on its slice for `EXPAND_DELAY`, or at once when it
    /// moves outward on it, even straight past its edge. Keeps it open while the pointer is on its ring or crossing the
    /// gap to it; another slice, the hole or going round the outside away from the ring folds it away.
    fn track_groups(&mut self, hit: Hit, at: Option<Pt>) {
        let center = self.center();
        let mut sub = None;
        match hit {
            Hit::Segment(i) if Some(i) == self.expanded => self.ring_from_keys = false,
            Hit::Segment(_) if at.is_some_and(|p| self.on_way_to_ring(p)) => self.clear_pending(),
            Hit::Segment(i) if self.can_expand(i) => {
                self.expanded = None;
                let distance = at.map_or(0.0, |p| p.dist(center));
                if self.pending_group != Some(i) {
                    self.clear_pending();
                    self.pending_group = Some(i);
                    self.pending_distance = distance;
                } else if at.is_some() && distance - self.pending_distance >= OUTWARD_PIXELS {
                    self.expand_group(i);
                } else if at.is_some() {
                    self.pending_distance = self.pending_distance.min(distance);
                }
            }
            Hit::SubSegment(_, k) => {
                self.ring_from_keys = false;
                sub = Some(k);
            }
            Hit::Outside
                if self.pending_group.is_some_and(|g| {
                    at.is_some_and(|p| self.in_fan(p, g) && p.dist(center) <= self.sub_radii().1 + BACKDROP_MARGIN)
                }) =>
            {
                // Flicked straight out to where its ring opens: no need to wait.
                if let Some(g) = self.pending_group {
                    self.expand_group(g);
                }
            }
            Hit::Outside => {
                self.clear_pending();
                if let (Some(e), Some(q)) = (self.expanded, at) {
                    if !self.in_fan(q, e) {
                        self.expanded = None; // went round the outside, away from the ring
                    }
                }
            }
            Hit::Center if self.ring_from_keys => sub = self.sub_hover,
            _ => {
                self.expanded = None;
                self.ring_from_keys = false;
                self.clear_pending();
            }
        }
        self.set_sub(sub);
    }

    /// The pointer moved over the donut (click mode).
    pub fn pointer_moved(&mut self, p: Pt) {
        if self.keyboard_hold {
            let anchor = *self.key_anchor.get_or_insert(p);
            if p.dist(anchor) < KEYBOARD_JITTER {
                return;
            }
            self.keyboard_hold = false;
        }
        self.last_pointer = Some(p);
        let hit = self.hit_test(p);
        self.track_groups(hit, Some(p));
        self.set_hover(match hit {
            Hit::Segment(i) | Hit::SubSegment(i, _) => Some(i),
            _ => None,
        });
    }

    /// The pointer left the window, last seen at `last`.
    pub fn pointer_left(&mut self, last: Option<Pt>) {
        if last.is_some_and(|p| self.left_through_seam(p)) {
            return;
        }
        self.collapse_group();
        self.set_hover(None);
    }

    pub fn pointer_pressed(&mut self, p: Pt, right: bool) {
        let hit = self.hit_test(p);
        if self.mode == Mode::Click && right {
            match hit {
                Hit::Segment(i) => self.effects.push(Effect::Edit { index: i, sub: None }),
                Hit::SubSegment(i, k) => self.effects.push(Effect::Edit { index: i, sub: Some(k) }),
                _ => {}
            }
            return;
        }
        if !right {
            self.press = Some(if let Hit::Segment(i) = hit { Some(i) } else { None });
        }
    }

    /// The left button went up: acts only if the press was begun here (a click, not a drag across).
    pub fn pointer_released(&mut self, p: Pt) {
        if self.press.take().is_none() {
            return;
        }
        match self.hit_test(p) {
            Hit::Segment(i) if self.preset_at(i, None).is_some() => self.effects.push(Effect::Invoke { index: i, sub: None }),
            Hit::Segment(i) if self.can_expand(i) => {
                self.expand_group(i); // no waiting when clicked
            }
            Hit::Outside if self.expanded.is_some() => self.collapse_group(),
            Hit::SubSegment(i, k) if self.preset_at(i, Some(k)).is_some() => {
                self.effects.push(Effect::Invoke { index: i, sub: Some(k) })
            }
            Hit::Center => self.effects.push(Effect::Close),
            _ => {}
        }
    }

    // ------------------------------------------------------------------ drag

    /// A drag from outside is over `p` (donut coordinates): lights up what it rests on and opens group rings.
    pub fn set_external_hit(&mut self, p: Pt) -> Hit {
        self.last_external_at = Some(p);
        let hit = self.hit_test(p);
        self.track_groups(hit, Some(p));
        let idx = match hit {
            Hit::Segment(i) | Hit::SubSegment(i, _) => Some(i),
            _ => None,
        };
        self.set_external_highlight(idx, hit == Hit::Center);
        hit
    }

    fn set_external_highlight(&mut self, idx: Option<usize>, center_hot: bool) {
        if center_hot && !self.external_center_hot {
            self.center_pulse.velocity = 6.0; // a small nudge of the hub
        }
        self.external_highlight = idx;
        self.external_center_hot = center_hot;
    }

    /// A drag from outside left, last seen at `last`: clears what it lit up and folds the ring, unless it went out
    /// through a see-through seam (Windows reports a drag leaving when it crosses a transparent pixel).
    pub fn external_left(&mut self, last: Option<Pt>) {
        if last.is_some_and(|p| self.left_through_seam(p)) {
            return;
        }
        self.last_external_at = None;
        self.collapse_group();
        self.set_external_highlight(None, false);
    }

    /// True when the pointer "left" at `p` well inside the donut or the backdrop of the group that is open or about to
    /// open: it went through a see-through pixel, it didn't really leave.
    pub fn left_through_seam(&self, p: Pt) -> bool {
        let Some(group) = self.expanded.or(self.pending_group) else { return false };
        let d = p.dist(self.center());
        if d <= self.style.outer + POP_OUT - LEAVE_EDGE {
            return true;
        }
        let count = self.children(group).len();
        let reach = self.sub_radii().1 + BACKDROP_MARGIN;
        if count == 0 || d > reach - LEAVE_EDGE {
            return false;
        }
        let half = (self.sub_span_of(count) * count as f64 + 2.0 * BACKDROP_MARGIN / reach).min(TAU) / 2.0;
        let inside = half - wrap(angle_of(self.center(), p) - mid_angle(group, self.items.len())).abs();
        inside > 0.0 && (inside >= PI / 2.0 || d * inside.sin() >= LEAVE_EDGE)
    }

    /// The area the window must keep at alpha >= 1/255 (see `Backing`).
    pub fn backing(&self) -> Backing {
        if self.faces.is_none() {
            return Backing::Full;
        }
        let fan = self.expanded.or(self.pending_group).and_then(|g| {
            let count = self.children(g).len();
            (count > 0).then(|| {
                let reach = self.sub_radii().1 + BACKDROP_MARGIN;
                Fan {
                    mid: mid_angle(g, self.items.len()),
                    half_span: (self.sub_span_of(count) * count as f64 + 2.0 * BACKDROP_MARGIN / reach).min(TAU) / 2.0,
                    inner: self.style.outer,
                    reach,
                }
            })
        });
        Backing::Disc { radius: self.style.outer + POP_OUT, fan }
    }

    // -------------------------------------------------------------- keyboard

    /// A key in click mode; `name` is the hook-style name ("Tab", "F2", "Q") for the flip key. True: consumed.
    pub fn handle_key(&mut self, key: Key, name: &str) -> bool {
        if self.style.flip_key.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(name)) {
            // Also when there is nothing to flip to: the key never does something else.
            self.flip();
            return true;
        }
        if !self.handle_key_core(key) {
            return false;
        }
        // The keyboard has the donut now: a mouse resting where it was doesn't take it back; moving it on purpose does.
        self.keyboard_hold = true;
        self.key_anchor = self.last_pointer;
        true
    }

    fn handle_key_core(&mut self, key: Key) -> bool {
        let n = self.items.len();
        if n == 0 {
            return false;
        }
        let step: i64 = match key {
            Key::Right | Key::Down => 1,
            Key::Left | Key::Up => -1,
            _ => 0,
        };
        // Esc / Backspace fold an open ring first; only then does Esc close the donut.
        if self.expanded_children().is_some() && matches!(key, Key::Escape | Key::Backspace) {
            let g = self.expanded;
            self.collapse_group();
            self.set_hover(g);
            return true;
        }
        if let Some(count) = self.expanded_children().map(|c| c.len()) {
            if let Some(k) = self.sub_hover {
                if matches!(key, Key::Enter | Key::Space) {
                    let g = self.expanded.unwrap_or(0);
                    if self.preset_at(g, Some(k)).is_some() {
                        self.effects.push(Effect::Invoke { index: g, sub: Some(k) });
                    }
                    return true;
                }
                if step == 0 {
                    return false;
                }
                self.set_sub(Some(((k as i64 + step + count as i64) % count as i64) as usize));
                return true;
            }
        }
        let current = self.active_index();
        if matches!(key, Key::Enter | Key::Space) {
            if let Some(c) = current {
                if self.can_expand(c) {
                    self.expand_group(c);
                    self.set_sub(Some(0));
                    self.ring_from_keys = true;
                } else if self.preset_at(c, None).is_some() {
                    self.effects.push(Effect::Invoke { index: c, sub: None });
                }
                return true;
            }
        }
        if step == 0 {
            return false;
        }
        self.collapse_group();
        let next = match current {
            None if step > 0 => 0,
            None => n - 1,
            Some(c) => ((c as i64 + step + n as i64) % n as i64) as usize,
        };
        self.set_hover(Some(next));
        true
    }

    // ------------------------------------------------------------------ flip

    /// A turn of the wheel flips once per turn: events less than 300 ms after the last one belong to it.
    pub fn flip_from_wheel(&mut self, now_ms: u64) -> bool {
        if !self.style.flip_wheel {
            return false;
        }
        let quiet = self.last_wheel_ms.is_none_or(|t| now_ms.saturating_sub(t) >= WHEEL_QUIET_MS);
        self.last_wheel_ms = Some(now_ms);
        quiet && self.flip()
    }

    /// Turns the donut to its other face. False when there is nothing to turn to or it can't turn now.
    pub fn flip(&mut self) -> bool {
        if self.closing || self.faces.is_none() {
            return false;
        }
        if self.flip.is_some() {
            self.flip_queued = !self.flip_queued; // pressed again mid-turn: twice cancels
            return true;
        }
        if !self.can_flip() {
            return false;
        }
        if !self.style.motion {
            self.collapse_group();
            self.swap_faces();
            return true;
        }
        let n_old = self.items.len();
        let origin = self.flip_origin();
        let active = self.active_index();
        let delay_of = |mid: f64, is_origin: bool| {
            if is_origin {
                0.0
            } else {
                FLIP_STAGGER * wrap(mid - origin).abs() / PI
            }
        };
        let origin_old = index_at(origin, n_old);
        let old: Vec<OldSlice> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let mid = mid_angle(i, n_old);
                OldSlice {
                    item: item.clone(),
                    mid,
                    span: span(n_old),
                    open: self.open.get(i).copied().unwrap_or(1.0),
                    hover: self.hover.get(i).map_or(0.0, |h| h.value),
                    hot: Some(i) == active && item.enabled,
                    delay: delay_of(mid, Some(i) == origin_old),
                }
            })
            .collect();
        self.collapse_group();
        self.swap_faces();
        let n_new = self.items.len();
        let origin_new = index_at(origin, n_new);
        let new_delay = (0..n_new)
            .map(|j| {
                let mid = mid_angle(j, n_new);
                let mut d = delay_of(mid, Some(j) == origin_new);
                // Behind the old slices it replaces: it turns in after them.
                for o in &old {
                    if wrap(mid - o.mid).abs() < (span(n_new) + o.span) / 2.0 {
                        d = d.max(o.delay);
                    }
                }
                d
            })
            .collect();
        self.flip = Some(Flip { clock: 0.0, old, new_delay });
        true
    }

    fn flip_origin(&self) -> f64 {
        let c = self.center();
        if let Some(p) = self.last_external_at {
            return angle_of(c, p);
        }
        if let (false, Some(p)) = (self.keyboard_hold, self.last_pointer) {
            return angle_of(c, p);
        }
        self.active_index().map_or(0.0, |i| mid_angle(i, self.items.len()))
    }

    /// Puts the other face's slices in place now; hit testing and drops belong to it from here on.
    fn swap_faces(&mut self) {
        let (external, pointer, keyboard) = (self.last_external_at, self.last_pointer, self.keyboard_hold);
        let next = self.other_face().to_vec();
        self.back = !self.back;
        self.set_items(next);
        // What the pointer or the drag rests on lights up on the new face; the keyboard selection starts over.
        if let Some(p) = external {
            self.set_external_hit(p);
        } else if let (false, Some(p)) = (keyboard, pointer) {
            self.pointer_moved(p);
        }
    }

    fn step_flip(&mut self, dt: f64) -> bool {
        let Some(f) = &mut self.flip else { return false };
        f.clock += dt;
        if f.clock >= f.duration() {
            self.finish_flip();
            if std::mem::take(&mut self.flip_queued) {
                self.flip();
            }
            return self.flip.is_some();
        }
        true
    }

    fn finish_flip(&mut self) {
        self.flip = None;
    }

    // ------------------------------------------------------------------- hub

    pub fn hub(&self) -> Hub {
        let mut hub = Hub::default();
        let verb = if self.mode == Mode::Click { "donut.click_to" } else { "donut.drop_to" };
        hub.title = match &self.faces {
            None => i18n::t("donut.center_drag"),
            Some(f) => self.files_title().unwrap_or_else(|| f.profile_label.clone()),
        };
        if self.external_center_hot {
            hub.subtitle = i18n::t("donut.cancel");
            hub.subtitle_danger = true;
        } else if let Some((a, item)) = self.active_index().and_then(|a| self.items.get(a).map(|s| (a, s))) {
            let ring_child = self.expanded.filter(|&g| g == a).zip(self.sub_hover).and_then(|(g, k)| self.children(g).get(k));
            hub.subtitle = if item.is_group {
                match ring_child {
                    Some(c) if c.enabled => i18n::format(verb, &[&format!("{} \u{b7} {}", item.label, c.label)]),
                    Some(c) => c.reason.clone().unwrap_or_default(),
                    None if item.enabled => i18n::t("donut.pick_in_group"),
                    None => item.reason.clone().unwrap_or_default(),
                }
            } else if item.enabled {
                i18n::format(verb, &[&item.label])
            } else {
                item.reason.clone().unwrap_or_default()
            };
        } else if self.mode == Mode::Click {
            hub.subtitle = i18n::t("donut.close_hint");
        }
        if self.faces.is_some() && self.can_flip() && (self.style.flip_key.is_some() || self.style.flip_wheel) {
            let key = self.style.flip_key.clone().unwrap_or_default();
            let k = if self.back { "donut.flip_to_front" } else { "donut.flip_to_back" };
            hub.hint = i18n::format(k, &[&key]).trim().to_string();
            hub.hint_wheel = self.style.flip_key.is_none();
        }
        hub
    }

    /// "3 x PNG" for files of one type, "5 files" for a mix.
    fn files_title(&self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let mut exts: Vec<String> = self
            .files
            .iter()
            .map(|p| p.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default())
            .collect();
        exts.sort();
        exts.dedup();
        let count = self.files.len().to_string();
        Some(match exts.as_slice() {
            [e] if !e.is_empty() => i18n::format("donut.files_same", &[&count, e]),
            _ => i18n::format("donut.files_mixed", &[&count]),
        })
    }
}

/// At most 12 slices a face; a group's ring at most 6, and never a group.
fn sanitize(mut slices: Vec<Slice>) -> Vec<Slice> {
    slices.truncate(MAX_SLICES);
    for s in &mut slices {
        if s.is_group {
            s.children.retain(|c| !c.is_group);
            s.children.truncate(MAX_SUB);
        } else {
            s.children.clear();
        }
    }
    slices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SliceAction;

    fn slice(id: &str, enabled: bool) -> Slice {
        Slice {
            preset_id: id.into(),
            label: id.to_uppercase(),
            enabled,
            reason: (!enabled).then(|| format!("{id} off")),
            action: SliceAction::Cli,
            ..Default::default()
        }
    }

    fn group(id: &str, kids: &[&str]) -> Slice {
        Slice {
            is_group: true,
            action: SliceAction::Group,
            children: kids.iter().map(|k| slice(k, true)).collect(),
            enabled: !kids.is_empty(),
            ..slice(id, true)
        }
    }

    fn faces(front: Vec<Slice>, back: Vec<Slice>) -> Faces {
        Faces { profile_id: "p".into(), profile_label: "Profile".into(), front, back }
    }

    fn menu(front: Vec<Slice>, back: Vec<Slice>, mode: Mode) -> Menu {
        let mut m = Menu::new();
        m.load(faces(front, back), vec![PathBuf::from("a.png"), PathBuf::from("b.png")], mode);
        m.play_open();
        m
    }

    fn six() -> Vec<Slice> {
        ["a", "b", "c", "d", "e", "f"].iter().map(|i| slice(i, true)).collect()
    }

    fn run(m: &mut Menu, seconds: f64) {
        for _ in 0..(seconds * 60.0) as usize {
            m.advance(1.0 / 60.0);
        }
    }

    /// A point at `radius` from the donut centre along slice `i`'s spoke.
    fn at(m: &Menu, i: usize, radius: f64) -> Pt {
        point_at(m.center(), radius, mid_angle(i, m.items.len()))
    }

    #[test]
    fn style_reads_settings_and_clamps() {
        let p = Palette { corner_radius: 40.0, reduce_motion: true, ..Palette::default() };
        let s = Style::from_settings(
            &p,
            &serde_json::json!({"outerRadius": 999, "holeRatio": 0.5, "opacity": 0.01, "flipKey": " F2 ", "flipWithWheel": false}),
        );
        assert_eq!(s.outer, 240.0);
        assert_eq!(s.hole_ratio, 0.5);
        assert_eq!(s.opacity, 0.1);
        assert_eq!(s.corner, 14.0);
        assert_eq!(s.flip_key.as_deref(), Some("F2"));
        assert!(!s.flip_wheel && !s.motion);
        let d = Style::from_settings(&Palette::default(), &serde_json::Value::Null);
        assert_eq!((d.outer, d.opacity, d.corner), (150.0, 0.96, 8.0));
        assert_eq!(d.flip_key.as_deref(), Some("Tab"));
        // A reserved key falls back to Tab; an empty one means none.
        let r = Style::from_settings(&Palette::default(), &serde_json::json!({"flipKey": "Enter"}));
        assert_eq!(r.flip_key.as_deref(), Some("Tab"));
        let n = Style::from_settings(&Palette::default(), &serde_json::json!({"flipKey": ""}));
        assert_eq!(n.flip_key, None);
    }

    #[test]
    fn load_limits_slices_and_rings() {
        let many: Vec<Slice> = (0..20).map(|i| slice(&format!("s{i}"), true)).collect();
        let mut g = group("g", &["1", "2", "3", "4", "5", "6", "7", "8"]);
        g.children.push(group("nested", &["x"]));
        let mut m = menu(many, vec![g], Mode::Click);
        assert_eq!(m.items.len(), 12);
        assert!(m.items.iter().all(|s| s.children.is_empty()));
        m.load(faces(vec![group("g", &["1", "2", "3", "4", "5", "6", "7", "8"])], vec![]), vec![], Mode::Click);
        assert_eq!(m.children(0).len(), 6);
    }

    #[test]
    fn window_reserves_room_for_rings_only_when_a_face_has_a_group() {
        let plain = menu(six(), vec![], Mode::Click);
        assert_eq!(plain.side(), 342.0);
        let with_group = menu(vec![group("g", &["x"])], vec![], Mode::Click);
        assert_eq!(with_group.side(), 466.0);
        let on_back = menu(six(), vec![group("g", &["x"])], Mode::Click);
        assert_eq!(on_back.side(), 466.0);
        // Drag mode doesn't know the files yet: it always reserves the ring and keeps its size once they arrive.
        let mut d = Menu::new();
        d.reset_for_drag();
        assert_eq!(d.side(), 466.0);
        d.load(faces(six(), vec![]), vec![], Mode::Drag);
        assert_eq!(d.side(), 466.0);
    }

    #[test]
    fn hole_and_slices_hit_test_with_the_hole_ratio() {
        let m = menu(six(), vec![], Mode::Click);
        assert_eq!(m.inner(), 150.0 * 0.42);
        assert_eq!(m.hit_test(m.center()), Hit::Center);
        assert_eq!(m.hit_test(at(&m, 2, 110.0)), Hit::Segment(2));
        assert_eq!(m.hit_test(at(&m, 2, 200.0)), Hit::Outside);
    }

    #[test]
    fn hover_springs_up_and_settles_down() {
        let mut m = menu(six(), vec![], Mode::Click);
        run(&mut m, 1.0);
        m.pointer_moved(at(&m, 1, 110.0));
        assert_eq!(m.active_index(), Some(1));
        run(&mut m, 1.0);
        assert_eq!(m.hover[1].value, 1.0);
        assert_eq!(m.hover[0].value, 0.0);
        let centre = m.center();
        m.pointer_moved(centre);
        assert_eq!(m.active_index(), None);
        run(&mut m, 1.0);
        assert_eq!(m.hover[1].value, 0.0);
    }

    #[test]
    fn open_animation_staggers_and_finishes_at_one() {
        let mut m = menu(six(), vec![], Mode::Click);
        assert!(m.open.iter().all(|&o| o == 0.0));
        m.advance(0.1);
        assert!(m.open[0] > m.open[5], "slice 0 starts first");
        assert!(m.advance(0.1));
        run(&mut m, 1.0);
        assert!(m.open.iter().all(|&o| o == 1.0));
        assert!(!m.advance(1.0 / 60.0));
    }

    #[test]
    fn close_animation_ends_with_close_done() {
        let mut m = menu(six(), vec![], Mode::Click);
        run(&mut m, 1.0);
        m.take_effects();
        m.play_close();
        assert!(m.closing);
        m.advance(0.05);
        assert!(m.open[0] < 1.0);
        run(&mut m, 1.0);
        assert_eq!(m.take_effects(), vec![Effect::CloseDone]);
        assert!(!m.closing);
    }

    #[test]
    fn reduced_motion_skips_the_animations() {
        let mut m = Menu::new();
        m.style.motion = false;
        m.load(faces(six(), vec![slice("x", true)]), vec![], Mode::Click);
        m.play_open();
        assert!(m.open.iter().all(|&o| o == 1.0));
        m.pointer_moved(at(&m, 2, 110.0));
        assert!(!m.advance(1.0 / 60.0));
        assert_eq!(m.hover[2].value, 1.0);
        assert!(m.flip());
        assert!(m.flip.is_none(), "flip swaps at once");
        assert!(m.back);
        m.play_close();
        assert_eq!(m.take_effects(), vec![Effect::CloseDone]);
    }

    #[test]
    fn click_invokes_only_when_pressed_and_released_on_the_same_enabled_slice() {
        let mut items = six();
        items[3].enabled = false;
        let mut m = menu(items, vec![], Mode::Click);
        let p = at(&m, 1, 110.0);
        m.pointer_pressed(p, false);
        m.pointer_released(p);
        assert_eq!(m.take_effects(), vec![Effect::Invoke { index: 1, sub: None }]);
        // Disabled slice: nothing.
        let d = at(&m, 3, 110.0);
        m.pointer_pressed(d, false);
        m.pointer_released(d);
        assert!(m.take_effects().is_empty());
        // The hole closes.
        m.pointer_pressed(m.center(), false);
        m.pointer_released(m.center());
        assert_eq!(m.take_effects(), vec![Effect::Close]);
        // A release without a press (the click began elsewhere) does nothing.
        m.pointer_released(p);
        assert!(m.take_effects().is_empty());
    }

    #[test]
    fn right_click_asks_to_edit_the_slice() {
        let mut m = menu(six(), vec![], Mode::Click);
        m.pointer_pressed(at(&m, 4, 110.0), true);
        assert_eq!(m.take_effects(), vec![Effect::Edit { index: 4, sub: None }]);
        let mut d = menu(six(), vec![], Mode::Drag);
        d.pointer_pressed(at(&d, 4, 110.0), true);
        assert!(d.take_effects().is_empty());
    }

    #[test]
    fn drag_drops_only_on_enabled_non_group_slices() {
        let mut items = six();
        items[1].enabled = false;
        items[2] = group("g", &["x", "y"]);
        let mut m = menu(items, vec![], Mode::Drag);
        let drops = |m: &mut Menu, p: Pt| {
            let hit = m.set_external_hit(p);
            m.converts(hit)
        };
        let p = at(&m, 0, 110.0);
        assert!(drops(&mut m, p));
        let p = at(&m, 1, 110.0);
        assert!(!drops(&mut m, p), "disabled");
        let p = at(&m, 2, 110.0);
        assert!(!drops(&mut m, p), "group itself");
        let p = m.center();
        assert!(!drops(&mut m, p), "hole");
        let p = at(&m, 0, 200.0);
        assert!(!drops(&mut m, p), "outside");
    }

    #[test]
    fn drag_highlight_is_set_and_cleared() {
        let mut m = menu(six(), vec![], Mode::Drag);
        m.set_external_hit(at(&m, 3, 110.0));
        assert_eq!(m.active_index(), Some(3));
        let centre = m.center();
        m.set_external_hit(centre);
        assert_eq!(m.active_index(), None);
        assert!(m.external_center_hot);
        assert_eq!(m.hub().subtitle, i18n::t("donut.cancel"));
        assert!(m.hub().subtitle_danger);
        m.external_left(None);
        assert!(!m.external_center_hot);
    }

    #[test]
    fn group_ring_opens_after_the_delay_or_by_moving_outward() {
        let mut m = menu(vec![group("g", &["x", "y", "z"]), slice("b", true), slice("c", true), slice("d", true)], vec![], Mode::Drag);
        run(&mut m, 1.0);
        // Resting on the group slice: pending, then open after 160 ms.
        m.set_external_hit(at(&m, 0, 90.0));
        assert_eq!(m.expanded, None);
        m.advance(0.1);
        assert_eq!(m.expanded, None);
        m.advance(0.07);
        assert_eq!(m.expanded, Some(0));
        // Leaving for the hole folds it.
        let centre = m.center();
        m.set_external_hit(centre);
        assert_eq!(m.expanded, None);
        run(&mut m, 1.0);
        assert_eq!(m.shown_expanded, None);
        // Outward by 10 px opens at once.
        m.set_external_hit(at(&m, 0, 80.0));
        assert_eq!(m.expanded, None);
        m.set_external_hit(at(&m, 0, 92.0));
        assert_eq!(m.expanded, Some(0));
    }

    #[test]
    fn group_ring_slices_are_hit_and_drop_with_their_own_preset() {
        let mut m = menu(vec![group("g", &["x", "y", "z"]), slice("b", true), slice("c", true), slice("d", true)], vec![], Mode::Drag);
        m.set_external_hit(at(&m, 0, 80.0));
        m.set_external_hit(at(&m, 0, 92.0));
        assert_eq!(m.expanded, Some(0));
        let (a, b) = m.sub_radii();
        let ring = point_at(m.center(), (a + b) / 2.0, mid_angle(0, 4));
        let hit = m.set_external_hit(ring);
        assert_eq!(hit, Hit::SubSegment(0, 1));
        assert!(m.converts(hit));
        assert_eq!(m.preset_at(0, Some(1)).map(|s| s.preset_id.as_str()), Some("y"));
        assert_eq!(m.preset_at(0, None), None);
        assert_eq!(m.hub().subtitle, i18n::format("donut.drop_to", &["G \u{b7} Y"]));
        // The gap between the donut and the ring still counts as the group: it doesn't fold on the way.
        let gap = point_at(m.center(), 150.0 + POP_OUT + 3.0, mid_angle(0, 4));
        assert_eq!(m.hit_test(gap), Hit::Segment(0));
    }

    #[test]
    fn a_drag_leave_through_a_seam_is_ignored_but_a_real_one_clears() {
        let mut m = menu(vec![group("g", &["x", "y"]), slice("b", true)], vec![], Mode::Drag);
        m.set_external_hit(at(&m, 0, 90.0));
        assert_eq!(m.pending_group, Some(0));
        // Last seen well inside the donut: a seam. The pending group stays.
        m.external_left(Some(at(&m, 0, 100.0)));
        assert_eq!(m.pending_group, Some(0));
        // Last seen at the edge of the window area: a real leave.
        m.external_left(Some(Pt::new(m.center().x, m.center().y - 300.0)));
        assert_eq!(m.pending_group, None);
        assert_eq!(m.active_index(), None);
    }

    #[test]
    fn keyboard_walks_the_slices_and_invokes() {
        let mut items = six();
        items[1].enabled = false;
        let mut m = menu(items, vec![], Mode::Click);
        assert!(m.handle_key(Key::Right, "Right"));
        assert_eq!(m.active_index(), Some(0), "first press starts at slice 0");
        assert!(m.handle_key(Key::Right, "Right"));
        assert_eq!(m.active_index(), Some(1));
        // Enter on a disabled slice does nothing but is eaten.
        assert!(m.handle_key(Key::Enter, "Enter"));
        assert!(m.take_effects().is_empty());
        assert!(m.handle_key(Key::Left, "Left"));
        assert!(m.handle_key(Key::Left, "Left"));
        assert_eq!(m.active_index(), Some(5), "wraps round");
        assert!(m.handle_key(Key::Space, "Space"));
        assert_eq!(m.take_effects(), vec![Effect::Invoke { index: 5, sub: None }]);
        // Esc on the donut is left to the window (it closes); other keys too.
        assert!(!m.handle_key(Key::Escape, "Escape"));
        assert!(!m.handle_key(Key::Other, "Q"));
    }

    #[test]
    fn keyboard_starts_from_the_last_slice_with_up() {
        let mut m = menu(six(), vec![], Mode::Click);
        assert!(m.handle_key(Key::Up, "Up"));
        assert_eq!(m.active_index(), Some(5));
    }

    #[test]
    fn keyboard_holds_the_selection_against_a_nudged_mouse() {
        let mut m = menu(six(), vec![], Mode::Click);
        m.pointer_moved(at(&m, 3, 110.0));
        m.handle_key(Key::Right, "Right");
        assert_eq!(m.active_index(), Some(4));
        // A 2 px nudge doesn't take the donut back; a real move does.
        let p = at(&m, 3, 110.0);
        m.pointer_moved(Pt::new(p.x + 2.0, p.y));
        assert_eq!(m.active_index(), Some(4));
        m.pointer_moved(at(&m, 0, 110.0));
        assert_eq!(m.active_index(), Some(0));
    }

    #[test]
    fn keyboard_enters_a_group_ring_and_esc_folds_it_before_closing() {
        let mut m = menu(vec![group("g", &["x", "y", "z"]), slice("b", true), slice("c", true)], vec![], Mode::Click);
        m.handle_key(Key::Right, "Right");
        assert!(m.handle_key(Key::Enter, "Enter"));
        assert_eq!(m.expanded, Some(0));
        assert_eq!(m.sub_hover, Some(0));
        assert!(m.handle_key(Key::Right, "Right"));
        assert_eq!(m.sub_hover, Some(1));
        assert!(m.handle_key(Key::Left, "Left"));
        assert!(m.handle_key(Key::Left, "Left"));
        assert_eq!(m.sub_hover, Some(2), "wraps in the ring");
        assert!(m.handle_key(Key::Enter, "Enter"));
        assert_eq!(m.take_effects(), vec![Effect::Invoke { index: 0, sub: Some(2) }]);
        assert!(m.handle_key(Key::Escape, "Escape"), "Esc folds the ring first");
        assert_eq!(m.expanded, None);
        assert_eq!(m.active_index(), Some(0));
        assert!(!m.handle_key(Key::Escape, "Escape"), "then it closes");
    }

    #[test]
    fn clicking_a_group_opens_it_at_once() {
        let mut m = menu(vec![group("g", &["x", "y"]), slice("b", true)], vec![], Mode::Click);
        let p = at(&m, 0, 100.0);
        m.pointer_pressed(p, false);
        m.pointer_released(p);
        assert_eq!(m.expanded, Some(0));
        assert!(m.take_effects().is_empty());
        // Clicking past the ring folds it.
        let far = point_at(m.center(), 150.0 + 100.0, mid_angle(0, 2) + 3.0);
        m.pointer_pressed(far, false);
        m.pointer_released(far);
        assert_eq!(m.expanded, None);
    }

    #[test]
    fn flip_needs_an_enabled_slice_on_the_other_face() {
        let mut m = menu(six(), vec![], Mode::Click);
        assert!(!m.can_flip());
        assert!(!m.flip());
        let mut m = menu(six(), vec![slice("x", false)], Mode::Click);
        assert!(!m.can_flip());
        let mut m2 = menu(six(), vec![slice("x", false), slice("y", true)], Mode::Click);
        assert!(m2.can_flip());
        assert!(m2.flip());
        assert!(m2.back);
        assert_eq!(m2.items.len(), 2);
        assert!(!m.flip());
    }

    #[test]
    fn flip_turns_old_slices_out_then_new_slices_in_and_ends() {
        let mut m = menu(six(), vec![slice("x", true), slice("y", true), slice("z", true)], Mode::Click);
        run(&mut m, 1.0);
        m.pointer_moved(at(&m, 2, 110.0));
        assert!(m.flip());
        // The new face is in place at once (drops belong to it); the old one is drawn folding.
        assert_eq!(m.items.len(), 3);
        let f = m.flip.as_ref().expect("flipping");
        assert_eq!(f.old.len(), 6);
        assert_eq!(f.old_width(0), 1.0);
        assert_eq!(f.new_width(0), 0.0);
        // The slice under the pointer starts first.
        let origin = f.old.iter().position(|o| o.delay == 0.0).unwrap();
        assert_eq!(origin, 2);
        m.advance(0.07);
        let f = m.flip.as_ref().unwrap();
        assert_eq!(f.old_width(2), 0.0, "past half a turn it is gone");
        // The new slice at the pointer turns in behind its neighbours' old slices (delay 0.08 * 60/180).
        assert_eq!(f.new_width(1), 0.0);
        m.advance(0.04);
        assert!(m.flip.as_ref().unwrap().new_width(1) > 0.0);
        run(&mut m, 1.0);
        assert!(m.flip.is_none());
    }

    #[test]
    fn flipping_twice_during_a_turn_cancels_and_once_more_after_it_flips_back() {
        let mut m = menu(six(), vec![slice("x", true), slice("y", true)], Mode::Click);
        assert!(m.flip());
        assert!(m.flip(), "second press mid-turn is queued");
        assert!(m.flip(), "third press un-queues it");
        assert!(m.flip(), "fourth queues again");
        run(&mut m, 1.0);
        assert!(!m.back, "queued flip ran after the first turn: back on the front face");
        assert_eq!(m.items.len(), 6);
    }

    #[test]
    fn flip_key_is_eaten_even_when_there_is_nothing_to_flip() {
        let mut m = menu(six(), vec![], Mode::Click);
        assert!(m.handle_key(Key::Other, "tab"));
        assert!(!m.back);
        m.style.flip_key = Some("F2".into());
        assert!(!m.handle_key(Key::Other, "Tab"));
        assert!(m.handle_key(Key::Other, "F2"));
    }

    #[test]
    fn wheel_flips_once_per_turn() {
        let mut m = menu(six(), vec![slice("x", true)], Mode::Click);
        m.style.motion = false;
        assert!(m.flip_from_wheel(1000));
        assert!(m.back);
        // Notches of the same turn (less than 300 ms apart) don't flip again, even over a long spin.
        assert!(!m.flip_from_wheel(1100));
        assert!(!m.flip_from_wheel(1350));
        assert!(!m.flip_from_wheel(1600));
        assert!(m.back);
        // A new turn after a pause does.
        assert!(m.flip_from_wheel(2000));
        assert!(!m.back);
        m.style.flip_wheel = false;
        assert!(!m.flip_from_wheel(5000));
    }

    #[test]
    fn hub_text_follows_the_state() {
        let mut off = slice("off", false);
        off.reason = Some("nope".into());
        let mut m = menu(vec![slice("a", true), off, slice("c", true)], vec![slice("x", true)], Mode::Click);
        let hub = m.hub();
        assert_eq!(hub.title, i18n::format("donut.files_same", &["2", "PNG"]));
        assert_eq!(hub.subtitle, i18n::t("donut.close_hint"));
        assert_eq!(hub.hint, i18n::format("donut.flip_to_back", &["Tab"]).trim());
        assert!(!hub.hint_wheel);
        m.pointer_moved(at(&m, 0, 110.0));
        assert_eq!(m.hub().subtitle, i18n::format("donut.click_to", &["A"]));
        m.pointer_moved(at(&m, 1, 110.0));
        assert_eq!(m.hub().subtitle, "nope");
        m.back = true;
        assert_eq!(m.hub().hint, i18n::format("donut.flip_to_front", &["Tab"]).trim());
        m.back = false;
        m.style.flip_key = None;
        assert!(m.hub().hint_wheel);
        m.style.flip_wheel = false;
        assert_eq!(m.hub().hint, "");
        let mut d = Menu::new();
        d.reset_for_drag();
        assert_eq!(d.hub().title, i18n::t("donut.center_drag"));
        assert_eq!(d.hub().subtitle, "");
    }

    #[test]
    fn hub_title_counts_files_of_one_or_mixed_types() {
        let mut m = menu(six(), vec![], Mode::Click);
        m.files = vec![PathBuf::from("a.PNG"), PathBuf::from("b.png")];
        assert_eq!(m.files_title().as_deref(), Some("donut.files_same"));
        m.files.push(PathBuf::from("c.jpg"));
        assert_eq!(m.files_title().as_deref(), Some("donut.files_mixed"));
        m.files.clear();
        assert_eq!(m.files_title(), None);
    }

    #[test]
    fn backing_is_the_whole_window_until_files_arrive_then_the_disc_and_open_fan() {
        let mut m = Menu::new();
        m.reset_for_drag();
        assert_eq!(m.backing(), Backing::Full);
        m.load(faces(vec![group("g", &["x", "y"]), slice("b", true)], vec![]), vec![], Mode::Drag);
        match m.backing() {
            Backing::Disc { radius, fan } => {
                assert_eq!(radius, 157.0);
                assert!(fan.is_none());
            }
            b => panic!("{b:?}"),
        }
        m.set_external_hit(at(&m, 0, 90.0));
        match m.backing() {
            Backing::Disc { fan: Some(f), .. } => {
                assert_eq!(f.inner, 150.0);
                assert_eq!(f.reach, 150.0 + POP_OUT + SUB_GAP + SUB_BAND + BACKDROP_MARGIN);
            }
            b => panic!("a group about to open reserves its fan: {b:?}"),
        }
    }

    #[test]
    fn empty_donut_still_hits_the_ring() {
        let mut m = Menu::new();
        m.load(faces(vec![], vec![]), vec![], Mode::Click);
        assert_eq!(m.hit_test(Pt::new(m.center().x + 100.0, m.center().y)), Hit::EmptyRing);
        assert!(!m.converts(Hit::EmptyRing));
        assert!(!m.handle_key(Key::Right, "Right"));
    }
}
