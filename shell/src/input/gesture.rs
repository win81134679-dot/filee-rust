//! The gesture state machine (spec §1.3, upstream `GestureDetector.cs`). Pure: events and a scope predicate in,
//! outputs out. No clock, no threads; the hold timer is the caller's (`Output::HoldTimer` asks for it, then
//! `hold_elapsed(token)` answers).

use serde_json::Value;

use super::keys::{self, Button};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Drag,
    Hold,
    KeyChord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    FileManager,
    Anywhere,
}

/// One `triggers[]` entry of settings.json (`TriggerGesture`).
#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    pub id: String,
    pub enabled: bool,
    pub kind: Kind,
    /// `keys::CTRL | ALT | SHIFT | META`; compared for equality.
    pub modifiers: u8,
    pub button: Button,
    /// Only KeyChord uses it.
    pub key: Option<String>,
    /// 0 = system default.
    pub drag_threshold: i32,
    pub hold_ms: u32,
    pub scope: Scope,
}

impl Trigger {
    fn new(kind: Kind, modifiers: u8, button: Button, key: Option<&str>) -> Trigger {
        Trigger {
            id: String::new(),
            enabled: true,
            kind,
            modifiers,
            button,
            key: key.map(str::to_string),
            drag_threshold: 0,
            hold_ms: 450,
            scope: Scope::FileManager,
        }
    }

    /// Upstream's defaults: Ctrl + left drag, Ctrl+Alt+Space.
    pub fn defaults() -> Vec<Trigger> {
        vec![
            Trigger::new(Kind::Drag, keys::CTRL, Button::Left, None),
            Trigger::new(Kind::KeyChord, keys::CTRL | keys::ALT, Button::None, Some("Space")),
        ]
    }

    fn from_json(v: &Value) -> Trigger {
        let kind = match v.get("kind") {
            Some(Value::String(s)) if s.eq_ignore_ascii_case("hold") => Kind::Hold,
            Some(Value::String(s)) if s.eq_ignore_ascii_case("keychord") => Kind::KeyChord,
            Some(Value::Number(n)) => match n.as_i64() {
                Some(1) => Kind::Hold,
                Some(2) => Kind::KeyChord,
                _ => Kind::Drag,
            },
            _ => Kind::Drag,
        };
        let scope = match v.get("scope") {
            Some(Value::String(s)) if s.eq_ignore_ascii_case("anywhere") => Scope::Anywhere,
            Some(Value::Number(n)) if n.as_i64() == Some(1) => Scope::Anywhere,
            _ => Scope::FileManager,
        };
        let modifiers = v.get("modifiers").map(keys::parse_modifiers).unwrap_or(keys::CTRL);
        let button = match v.get("button") {
            Some(b) => Button::parse(b),
            None => Button::Left,
        };
        Trigger {
            id: v.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            kind,
            modifiers,
            button,
            key: v.get("key").and_then(Value::as_str).map(str::to_string).filter(|k| !k.trim().is_empty()),
            drag_threshold: v.get("dragThreshold").and_then(Value::as_i64).unwrap_or(0).clamp(0, 10_000) as i32,
            hold_ms: v.get("holdMilliseconds").and_then(Value::as_i64).unwrap_or(450).clamp(1, 60_000) as u32,
            scope,
        }
    }

    /// The `triggers` array of settings.json; upstream's defaults when the key is missing (an empty array stays empty).
    pub fn parse_list(settings: &Value) -> Vec<Trigger> {
        match settings.get("triggers").and_then(Value::as_array) {
            Some(list) => list.iter().map(Trigger::from_json).collect(),
            None => Trigger::defaults(),
        }
    }
}

/// What the detector wants done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// A drag passed its threshold (or its modifier came late): `InputEvent::DragStarted`.
    DragStarted { x: i32, y: i32 },
    /// A drag was active and a mouse button went up: `InputEvent::DragEnded`.
    DragEnded { x: i32, y: i32 },
    /// A hold or key chord fired: `InputEvent::SelectionGesture`.
    Selection { x: i32, y: i32 },
    /// Call `hold_elapsed(token)` after `ms` milliseconds.
    HoldTimer { token: u64, ms: u32 },
}

#[derive(Clone, Copy, Debug)]
struct Armed {
    drag: Option<usize>,
    hold: Option<usize>,
    x: i32,
    y: i32,
    token: u64,
}

/// The scope predicate: may this gesture start at this point? Called while the detector decides, so it must be fast
/// (the worker thread runs it, never the OS hook callback).
pub type ScopeFn<'a> = dyn FnMut(&Trigger, i32, i32) -> bool + 'a;

#[derive(Default)]
pub struct Detector {
    gestures: Vec<Trigger>,
    armed: Option<Armed>,
    drag_active: bool,
    late_rejected: bool,
    last: (i32, i32),
    pub paused: bool,
    /// Fallback threshold (physical pixels) for gestures with `drag_threshold == 0`.
    pub system_threshold: i32,
}

impl Detector {
    pub fn new(system_threshold: i32) -> Detector {
        Detector { system_threshold, ..Default::default() }
    }

    /// New gesture list (disabled ones are dropped); forgets a half-armed press.
    pub fn configure(&mut self, gestures: &[Trigger]) {
        self.gestures = gestures.iter().filter(|g| g.enabled).cloned().collect();
        self.armed = None;
    }

    /// Seeds the key-chord position before any mouse event was seen.
    pub fn set_last_position(&mut self, x: i32, y: i32) {
        self.last = (x, y);
    }

    fn first_in_scope(&self, kind: Kind, button: Button, mods: u8, x: i32, y: i32, scope: &mut ScopeFn) -> Option<usize> {
        let found = self
            .gestures
            .iter()
            .position(|g| g.kind == kind && g.button == button && g.modifiers == mods && scope(g, x, y));
        found
    }

    pub fn mouse_down(&mut self, button: Button, x: i32, y: i32, mods: u8, now_ms: u64, scope: &mut ScopeFn) -> Vec<Output> {
        self.last = (x, y);
        self.armed = None;
        self.late_rejected = false;
        if self.paused {
            return Vec::new();
        }
        let drag = self.first_in_scope(Kind::Drag, button, mods, x, y, scope);
        let hold = self.first_in_scope(Kind::Hold, button, mods, x, y, scope);
        if drag.is_none() && hold.is_none() {
            return Vec::new();
        }
        let token = now_ms ^ (x as i64).wrapping_mul(7919) as u64 ^ (y as i64) as u64;
        self.armed = Some(Armed { drag, hold, x, y, token });
        match hold {
            Some(h) => vec![Output::HoldTimer { token, ms: self.gestures[h].hold_ms }],
            None => Vec::new(),
        }
    }

    pub fn mouse_move(&mut self, x: i32, y: i32, mods: u8, held: Button, scope: &mut ScopeFn) -> Vec<Output> {
        self.last = (x, y);
        let fired = if self.armed.is_none() {
            self.detect_late_modifier(x, y, mods, held, scope)
        } else {
            self.detect_drag_past_threshold(x, y)
        };
        if fired {
            self.drag_active = true;
            vec![Output::DragStarted { x, y }]
        } else {
            Vec::new()
        }
    }

    fn moved(&self, gesture: &Trigger, armed: &Armed, x: i32, y: i32) -> bool {
        let t = if gesture.drag_threshold > 0 { gesture.drag_threshold } else { self.system_threshold };
        (x - armed.x).abs() > t || (y - armed.y).abs() > t
    }

    fn detect_drag_past_threshold(&mut self, x: i32, y: i32) -> bool {
        let mut armed = match self.armed {
            Some(a) => a,
            None => return false,
        };
        if let Some(h) = armed.hold {
            if self.moved(&self.gestures[h], &armed, x, y) {
                armed.hold = None;
            }
        }
        if let Some(d) = armed.drag {
            if self.moved(&self.gestures[d], &armed, x, y) {
                self.armed = None;
                return true;
            }
        }
        self.armed = if armed.drag.is_none() && armed.hold.is_none() { None } else { Some(armed) };
        false
    }

    fn detect_late_modifier(&mut self, x: i32, y: i32, mods: u8, held: Button, scope: &mut ScopeFn) -> bool {
        if self.drag_active || self.late_rejected || self.paused || held == Button::None || mods == 0 {
            return false;
        }
        let late = match self
            .gestures
            .iter()
            .position(|g| g.kind == Kind::Drag && g.button == held && g.modifiers == mods)
        {
            Some(i) => i,
            None => return false,
        };
        if scope(&self.gestures[late], x, y) {
            return true;
        }
        self.late_rejected = true;
        false
    }

    /// Any mouse button went up (the button isn't compared, like upstream).
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Vec<Output> {
        self.last = (x, y);
        self.armed = None;
        let ended = self.drag_active;
        self.drag_active = false;
        self.late_rejected = false;
        if ended {
            vec![Output::DragEnded { x, y }]
        } else {
            Vec::new()
        }
    }

    /// The hold timer for `token` ran out. Does not look at `paused` (the app does).
    pub fn hold_elapsed(&mut self, token: u64) -> Vec<Output> {
        if let Some(armed) = self.armed {
            if armed.hold.is_some() && armed.token == token {
                self.armed = None;
                self.late_rejected = true;
                return vec![Output::Selection { x: armed.x, y: armed.y }];
            }
        }
        Vec::new()
    }

    /// A key went down; `key` is the name from `keys::key_name`. Fires at the last mouse position.
    pub fn key_down(&mut self, key: &str, mods: u8, scope: &mut ScopeFn) -> Vec<Output> {
        if self.paused {
            return Vec::new();
        }
        let (x, y) = self.last;
        let hit = self.gestures.iter().any(|g| {
            g.kind == Kind::KeyChord
                && g.key.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(key))
                && g.modifiers == mods
                && scope(g, x, y)
        });
        if hit {
            vec![Output::Selection { x, y }]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keys::{ALT, CTRL, SHIFT};
    use serde_json::json;

    fn drag(mods: u8, button: Button) -> Trigger {
        Trigger::new(Kind::Drag, mods, button, None)
    }
    fn hold(mods: u8, button: Button, ms: u32) -> Trigger {
        Trigger { hold_ms: ms, ..Trigger::new(Kind::Hold, mods, button, None) }
    }
    fn chord(mods: u8, key: &str) -> Trigger {
        Trigger::new(Kind::KeyChord, mods, Button::None, Some(key))
    }
    fn det(gestures: &[Trigger]) -> Detector {
        let mut d = Detector::new(4);
        d.configure(gestures);
        d
    }
    fn yes(_: &Trigger, _: i32, _: i32) -> bool {
        true
    }
    fn no(_: &Trigger, _: i32, _: i32) -> bool {
        false
    }

    // ---- drag ----

    #[test]
    fn drag_fires_only_strictly_past_threshold_per_axis() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        assert!(d.mouse_down(Button::Left, 100, 100, CTRL, 0, &mut yes).is_empty());
        // exactly the threshold on either axis does not fire
        assert!(d.mouse_move(104, 100, CTRL, Button::Left, &mut yes).is_empty());
        assert!(d.mouse_move(100, 96, CTRL, Button::Left, &mut yes).is_empty());
        assert!(d.mouse_move(104, 104, CTRL, Button::Left, &mut yes).is_empty());
        // 5 on one axis fires, at the position of that move event
        assert_eq!(
            d.mouse_move(105, 100, CTRL, Button::Left, &mut yes),
            vec![Output::DragStarted { x: 105, y: 100 }]
        );
    }

    #[test]
    fn threshold_is_per_axis_not_euclidean() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        // (4,4) has distance 5.6 > 4 but neither axis is > 4
        assert!(d.mouse_move(4, 4, CTRL, Button::Left, &mut yes).is_empty());
        assert!(d.mouse_move(-4, 4, CTRL, Button::Left, &mut yes).is_empty());
        assert_eq!(d.mouse_move(0, -5, CTRL, Button::Left, &mut yes).len(), 1);
    }

    #[test]
    fn gesture_threshold_overrides_system_threshold() {
        let g = Trigger { drag_threshold: 20, ..drag(CTRL, Button::Left) };
        let mut d = det(&[g]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        assert!(d.mouse_move(20, 0, CTRL, Button::Left, &mut yes).is_empty());
        assert_eq!(d.mouse_move(21, 0, CTRL, Button::Left, &mut yes).len(), 1);
    }

    #[test]
    fn drag_ends_on_any_button_up_once() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        d.mouse_move(50, 0, CTRL, Button::Left, &mut yes);
        // moving more doesn't start another drag
        assert!(d.mouse_move(90, 0, CTRL, Button::Left, &mut yes).is_empty());
        assert_eq!(d.mouse_up(90, 3), vec![Output::DragEnded { x: 90, y: 3 }]);
        assert!(d.mouse_up(90, 3).is_empty());
    }

    #[test]
    fn up_without_drag_gives_no_end() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        assert!(d.mouse_up(1, 1).is_empty());
        // and the press is disarmed: moving afterwards (button no longer held) does nothing
        assert!(d.mouse_move(100, 100, CTRL, Button::None, &mut yes).is_empty());
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        assert!(d.mouse_down(Button::Left, 0, 0, CTRL | SHIFT, 0, &mut yes).is_empty());
        assert!(d.mouse_move(50, 0, CTRL | SHIFT, Button::None, &mut yes).is_empty());
        d.mouse_down(Button::Left, 0, 0, 0, 0, &mut yes);
        assert!(d.mouse_move(50, 0, 0, Button::Left, &mut yes).is_empty());
        d.mouse_down(Button::Right, 0, 0, CTRL, 0, &mut yes);
        assert!(d.mouse_move(50, 0, CTRL, Button::Right, &mut yes).is_empty());
    }

    #[test]
    fn disabled_gestures_are_ignored() {
        let g = Trigger { enabled: false, ..drag(CTRL, Button::Left) };
        let mut d = det(&[g]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        assert!(d.mouse_move(50, 0, CTRL, Button::Left, &mut yes).is_empty());
    }

    #[test]
    fn scope_failing_at_press_does_not_arm() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut no);
        // no arm, so the move goes to the late-modifier path; scope still fails and rejects
        assert!(d.mouse_move(50, 0, CTRL, Button::Left, &mut no).is_empty());
    }

    // ---- late modifier ----

    #[test]
    fn late_modifier_fires_on_first_move_with_the_modifier() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        // pressed without Ctrl: nothing armed
        assert!(d.mouse_down(Button::Left, 10, 10, 0, 0, &mut yes).is_empty());
        assert!(d.mouse_move(60, 10, 0, Button::Left, &mut yes).is_empty());
        // Ctrl arrives mid-drag: fires immediately, at that move's position
        assert_eq!(
            d.mouse_move(61, 10, CTRL, Button::Left, &mut yes),
            vec![Output::DragStarted { x: 61, y: 10 }]
        );
        // only once
        assert!(d.mouse_move(62, 10, CTRL, Button::Left, &mut yes).is_empty());
        assert_eq!(d.mouse_up(62, 10).len(), 1);
    }

    #[test]
    fn late_modifier_needs_a_held_button_and_modifiers() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        assert!(d.mouse_move(5, 5, CTRL, Button::None, &mut yes).is_empty());
        assert!(d.mouse_move(6, 5, 0, Button::Left, &mut yes).is_empty());
        assert!(d.mouse_move(7, 5, CTRL, Button::Right, &mut yes).is_empty());
    }

    #[test]
    fn late_modifier_scope_rejection_sticks_until_button_up() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        d.mouse_down(Button::Left, 10, 10, 0, 0, &mut yes);
        assert!(d.mouse_move(20, 10, CTRL, Button::Left, &mut no).is_empty());
        // even if the pointer moves into scope, this press stays rejected
        assert!(d.mouse_move(30, 10, CTRL, Button::Left, &mut yes).is_empty());
        d.mouse_up(30, 10);
        d.mouse_down(Button::Left, 10, 10, 0, 0, &mut yes);
        assert_eq!(d.mouse_move(31, 10, CTRL, Button::Left, &mut yes).len(), 1);
    }

    #[test]
    fn late_modifier_works_without_a_mouse_down_seen() {
        // pointer entering scope with the button already held
        let mut d = det(&[drag(CTRL, Button::Left)]);
        assert_eq!(d.mouse_move(5, 5, CTRL, Button::Left, &mut yes).len(), 1);
    }

    // ---- hold ----

    #[test]
    fn hold_fires_at_press_position_after_timer() {
        let mut d = det(&[hold(CTRL, Button::Left, 500)]);
        let out = d.mouse_down(Button::Left, 30, 40, CTRL, 123, &mut yes);
        let token = match out[..] {
            [Output::HoldTimer { token, ms }] => {
                assert_eq!(ms, 500);
                token
            }
            _ => panic!("expected a timer request, got {out:?}"),
        };
        // small jitter keeps the hold
        assert!(d.mouse_move(33, 42, CTRL, Button::Left, &mut yes).is_empty());
        assert_eq!(d.hold_elapsed(token), vec![Output::Selection { x: 30, y: 40 }]);
        // fires once
        assert!(d.hold_elapsed(token).is_empty());
    }

    #[test]
    fn hold_is_cancelled_by_moving_past_threshold() {
        let mut d = det(&[hold(CTRL, Button::Left, 500)]);
        let token = match d.mouse_down(Button::Left, 0, 0, CTRL, 1, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        assert!(d.mouse_move(10, 0, CTRL, Button::Left, &mut yes).is_empty());
        assert!(d.hold_elapsed(token).is_empty());
    }

    #[test]
    fn hold_is_cancelled_by_release_and_by_a_new_press() {
        let mut d = det(&[hold(CTRL, Button::Left, 500)]);
        let t1 = match d.mouse_down(Button::Left, 0, 0, CTRL, 1, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        d.mouse_up(0, 0);
        assert!(d.hold_elapsed(t1).is_empty());
        let t2 = match d.mouse_down(Button::Left, 0, 0, CTRL, 2, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        assert_ne!(t1, t2);
        let _t3 = d.mouse_down(Button::Left, 0, 0, CTRL, 3, &mut yes);
        assert!(d.hold_elapsed(t2).is_empty());
    }

    #[test]
    fn hold_token_must_match() {
        let mut d = det(&[hold(CTRL, Button::Left, 500)]);
        let token = match d.mouse_down(Button::Left, 7, 9, CTRL, 55, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        assert!(d.hold_elapsed(token ^ 1).is_empty());
        assert_eq!(d.hold_elapsed(token).len(), 1);
    }

    #[test]
    fn after_a_hold_moving_does_not_start_the_drag_that_shares_its_modifiers() {
        let mut d = det(&[drag(CTRL, Button::Left), hold(CTRL, Button::Left, 300)]);
        let token = match d.mouse_down(Button::Left, 0, 0, CTRL, 9, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        assert_eq!(d.hold_elapsed(token).len(), 1);
        assert!(d.mouse_move(100, 100, CTRL, Button::Left, &mut yes).is_empty());
        assert!(d.mouse_move(200, 100, CTRL, Button::Left, &mut yes).is_empty());
    }

    #[test]
    fn drag_and_hold_share_a_press_moving_wins_the_drag_and_cancels_hold() {
        let mut d = det(&[drag(CTRL, Button::Left), hold(CTRL, Button::Left, 300)]);
        let token = match d.mouse_down(Button::Left, 0, 0, CTRL, 9, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        assert_eq!(d.mouse_move(10, 0, CTRL, Button::Left, &mut yes), vec![Output::DragStarted { x: 10, y: 0 }]);
        assert!(d.hold_elapsed(token).is_empty());
    }

    #[test]
    fn hold_without_drag_is_disarmed_after_moving_so_nothing_else_fires() {
        let mut d = det(&[hold(CTRL, Button::Left, 300)]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 9, &mut yes);
        assert!(d.mouse_move(50, 0, CTRL, Button::Left, &mut yes).is_empty());
        // armed is now None; the move path becomes the late-modifier check, which has no Drag gesture
        assert!(d.mouse_move(60, 0, CTRL, Button::Left, &mut yes).is_empty());
    }

    #[test]
    fn hold_threshold_uses_the_system_default_when_its_own_is_zero() {
        let mut d = det(&[hold(CTRL, Button::Left, 300)]);
        let token = match d.mouse_down(Button::Left, 0, 0, CTRL, 9, &mut yes)[0] {
            Output::HoldTimer { token, .. } => token,
            _ => unreachable!(),
        };
        assert!(d.mouse_move(4, 4, CTRL, Button::Left, &mut yes).is_empty()); // not > 4
        assert_eq!(d.hold_elapsed(token).len(), 1);
    }

    // ---- key chord ----

    #[test]
    fn key_chord_needs_exact_modifiers_and_key_case_insensitive() {
        let mut d = det(&[chord(CTRL | ALT, "Space")]);
        d.mouse_move(300, 200, 0, Button::None, &mut yes);
        assert_eq!(d.key_down("Space", CTRL | ALT, &mut yes), vec![Output::Selection { x: 300, y: 200 }]);
        assert_eq!(d.key_down("space", CTRL | ALT, &mut yes).len(), 1);
        assert!(d.key_down("Space", CTRL, &mut yes).is_empty());
        assert!(d.key_down("Space", CTRL | ALT | SHIFT, &mut yes).is_empty());
        assert!(d.key_down("Tab", CTRL | ALT, &mut yes).is_empty());
    }

    #[test]
    fn key_chord_position_is_zero_without_mouse_events_and_seedable() {
        let mut d = det(&[chord(CTRL, "Q")]);
        assert_eq!(d.key_down("Q", CTRL, &mut yes), vec![Output::Selection { x: 0, y: 0 }]);
        d.set_last_position(12, 34);
        assert_eq!(d.key_down("Q", CTRL, &mut yes), vec![Output::Selection { x: 12, y: 34 }]);
    }

    #[test]
    fn key_chord_uses_the_last_position_from_any_mouse_event() {
        let mut d = det(&[chord(CTRL, "Q")]);
        d.mouse_down(Button::Left, 5, 6, 0, 0, &mut yes);
        assert_eq!(d.key_down("Q", CTRL, &mut yes), vec![Output::Selection { x: 5, y: 6 }]);
        d.mouse_up(8, 9);
        assert_eq!(d.key_down("Q", CTRL, &mut yes), vec![Output::Selection { x: 8, y: 9 }]);
    }

    #[test]
    fn key_chord_respects_scope_and_picks_the_first_in_scope() {
        let a = Trigger { scope: Scope::FileManager, id: "a".into(), ..chord(CTRL, "Q") };
        let b = Trigger { scope: Scope::Anywhere, id: "b".into(), ..chord(CTRL, "Q") };
        let mut d = det(&[a, b]);
        let mut only_b = |g: &Trigger, _: i32, _: i32| g.id == "b";
        assert_eq!(d.key_down("Q", CTRL, &mut only_b).len(), 1);
        assert!(d.key_down("Q", CTRL, &mut no).is_empty());
    }

    // ---- paused, configure, several triggers ----

    #[test]
    fn paused_detects_nothing() {
        let mut d = det(&[drag(CTRL, Button::Left), hold(CTRL, Button::Right, 100), chord(CTRL, "Q")]);
        d.paused = true;
        assert!(d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes).is_empty());
        assert!(d.mouse_move(90, 0, CTRL, Button::Left, &mut yes).is_empty()); // late modifier path
        assert!(d.mouse_down(Button::Right, 0, 0, CTRL, 0, &mut yes).is_empty());
        assert!(d.key_down("Q", CTRL, &mut yes).is_empty());
    }

    #[test]
    fn pausing_after_arming_blocks_the_late_path_but_upstream_still_lets_the_press_finish() {
        let mut d = det(&[drag(CTRL, Button::Left)]);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        d.paused = true;
        // the armed press still resolves (upstream checks Paused only at press, key and late-modifier time)
        assert_eq!(d.mouse_move(50, 0, CTRL, Button::Left, &mut yes).len(), 1);
        // but the end is still reported so the donut can close
        assert_eq!(d.mouse_up(50, 0).len(), 1);
    }

    #[test]
    fn configure_forgets_an_armed_press() {
        let g = [drag(CTRL, Button::Left)];
        let mut d = det(&g);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        d.configure(&g);
        // not armed any more: the move goes through the late-modifier path, and Ctrl is held, so it still fires there
        assert_eq!(d.mouse_move(50, 0, CTRL, Button::Left, &mut yes).len(), 1);
        // without the modifier it would not
        d.mouse_up(50, 0);
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        d.configure(&g);
        assert!(d.mouse_move(50, 0, 0, Button::Left, &mut yes).is_empty());
    }

    #[test]
    fn several_triggers_pick_by_button_and_modifiers_in_list_order() {
        let first = Trigger { id: "first".into(), ..drag(CTRL, Button::Left) };
        let second = Trigger { id: "second".into(), drag_threshold: 30, ..drag(CTRL, Button::Left) };
        let alt = Trigger { id: "alt".into(), drag_threshold: 10, ..drag(ALT, Button::Left) };
        let mut d = det(&[first, second, alt]);
        // Ctrl press: the first Ctrl gesture (threshold 4) wins
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut yes);
        assert_eq!(d.mouse_move(5, 0, CTRL, Button::Left, &mut yes).len(), 1);
        d.mouse_up(5, 0);
        // Alt press: its own threshold (10)
        d.mouse_down(Button::Left, 0, 0, ALT, 0, &mut yes);
        assert!(d.mouse_move(10, 0, ALT, Button::Left, &mut yes).is_empty());
        assert_eq!(d.mouse_move(11, 0, ALT, Button::Left, &mut yes).len(), 1);
    }

    #[test]
    fn scope_is_asked_per_gesture() {
        let first = Trigger { id: "first".into(), ..drag(CTRL, Button::Left) };
        let second = Trigger { id: "second".into(), drag_threshold: 30, ..drag(CTRL, Button::Left) };
        let mut d = det(&[first, second]);
        let mut only_second = |g: &Trigger, _: i32, _: i32| g.id == "second";
        d.mouse_down(Button::Left, 0, 0, CTRL, 0, &mut only_second);
        assert!(d.mouse_move(10, 0, CTRL, Button::Left, &mut yes).is_empty()); // second's threshold is 30
        assert_eq!(d.mouse_move(31, 0, CTRL, Button::Left, &mut yes).len(), 1);
    }

    // ---- settings parsing ----

    #[test]
    fn parses_the_local_settings_shape() {
        let settings = json!({
            "triggers": [
                {"id":"5c35e34a","enabled":true,"kind":"Drag","modifiers":"Ctrl","button":"Left","dragThreshold":0,"holdMilliseconds":450,"scope":"FileManager"},
                {"id":"be4676e1","enabled":true,"kind":"KeyChord","modifiers":"Ctrl, Alt","button":"None","key":"Space","dragThreshold":0,"holdMilliseconds":450,"scope":"FileManager"},
                {"id":"x","enabled":false,"kind":"Hold","modifiers":"Shift","button":"Right","holdMilliseconds":800,"scope":"Anywhere"}
            ]
        });
        let list = Trigger::parse_list(&settings);
        assert_eq!(list.len(), 3);
        assert_eq!((list[0].kind, list[0].modifiers, list[0].button), (Kind::Drag, CTRL, Button::Left));
        assert_eq!(list[1].kind, Kind::KeyChord);
        assert_eq!(list[1].modifiers, CTRL | ALT);
        assert_eq!(list[1].key.as_deref(), Some("Space"));
        assert!(!list[2].enabled);
        assert_eq!((list[2].kind, list[2].scope, list[2].hold_ms), (Kind::Hold, Scope::Anywhere, 800));
    }

    #[test]
    fn missing_triggers_means_defaults_empty_means_none() {
        assert_eq!(Trigger::parse_list(&json!({})), Trigger::defaults());
        assert!(Trigger::parse_list(&json!({"triggers": []})).is_empty());
        let d = Trigger::defaults();
        assert_eq!((d[0].kind, d[0].modifiers, d[0].button), (Kind::Drag, CTRL, Button::Left));
        assert_eq!((d[1].kind, d[1].modifiers, d[1].key.as_deref()), (Kind::KeyChord, CTRL | ALT, Some("Space")));
    }
}
