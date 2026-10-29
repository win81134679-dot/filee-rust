//! The worker's brain, without Win32: raw hook events and the clock in, `InputEvent`s out. Owns the gesture detector,
//! the hold timer, the flip key's auto-repeat filter and the wheel debounce. The worker thread (hook.rs) feeds it
//! and supplies the scope predicate, so everything here is testable.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::gesture::{Detector, Kind, Output, ScopeFn, Trigger};
use super::keys::{self, Button};
use crate::model::InputEvent;

/// Wheel events closer together than this are one turn of the wheel (`DonutMenu.WheelQuietMs`).
pub const WHEEL_QUIET: Duration = Duration::from_millis(300);

/// What the hook callbacks record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Raw {
    Move { x: i32, y: i32, mods: u8, held: Button },
    Down { button: Button, x: i32, y: i32, mods: u8 },
    Up { x: i32, y: i32 },
    Key { vk: u32, down: bool, mods: u8 },
    /// One notch of the vertical wheel.
    Wheel,
}

/// The slice of settings.json the input module uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub triggers: Vec<Trigger>,
    pub paused: bool,
    pub excluded: Vec<String>,
    /// The key that flips the donut now (`EffectiveFlipKey`), if any.
    pub flip_key: Option<String>,
    pub flip_with_wheel: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config::from_settings(&Value::Null)
    }
}

impl Config {
    pub fn from_settings(settings: &Value) -> Config {
        let triggers = Trigger::parse_list(settings);
        let chord_keys: Vec<String> =
            triggers.iter().filter(|t| t.kind == Kind::KeyChord).filter_map(|t| t.key.clone()).collect();
        let donut = settings.get("donut");
        let flip_key_setting = match donut.and_then(|d| d.get("flipKey")) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => keys::DEFAULT_FLIP_KEY.to_string(),
            Some(_) => keys::DEFAULT_FLIP_KEY.to_string(),
        };
        Config {
            triggers,
            paused: settings.get("paused").and_then(Value::as_bool).unwrap_or(false),
            excluded: settings
                .get("excludedProcesses")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            flip_key: keys::effective_flip_key(&flip_key_setting, &chord_keys),
            flip_with_wheel: donut.and_then(|d| d.get("flipWithWheel")).and_then(Value::as_bool).unwrap_or(true),
        }
    }
}

pub struct Engine {
    detector: Detector,
    flip_key: Option<String>,
    flip_with_wheel: bool,
    pending_hold: Option<(u64, Instant)>,
    down_keys: HashSet<u32>,
    last_wheel: Option<Instant>,
    epoch: Instant,
}

impl Engine {
    pub fn new(system_threshold: i32, epoch: Instant) -> Engine {
        Engine {
            detector: Detector::new(system_threshold),
            flip_key: None,
            flip_with_wheel: true,
            pending_hold: None,
            down_keys: HashSet::new(),
            last_wheel: None,
            epoch,
        }
    }

    pub fn configure(&mut self, cfg: &Config) {
        self.detector.configure(&cfg.triggers);
        self.detector.paused = cfg.paused;
        self.flip_key = cfg.flip_key.clone();
        self.flip_with_wheel = cfg.flip_with_wheel;
        self.pending_hold = None; // `configure` forgot the armed press the timer belonged to
    }

    pub fn set_last_position(&mut self, x: i32, y: i32) {
        self.detector.set_last_position(x, y);
    }

    /// When the hold timer is due, if one is running.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending_hold.map(|(_, at)| at)
    }

    /// Fires the hold timer if it ran out by `now`.
    pub fn tick(&mut self, now: Instant) -> Vec<InputEvent> {
        match self.pending_hold {
            Some((token, at)) if now >= at => {
                self.pending_hold = None;
                let outputs = self.detector.hold_elapsed(token);
                self.convert(outputs, now)
            }
            _ => Vec::new(),
        }
    }

    pub fn handle(&mut self, raw: Raw, now: Instant, watch_flip: bool, scope: &mut ScopeFn) -> Vec<InputEvent> {
        let mut out = self.tick(now);
        let now_ms = now.saturating_duration_since(self.epoch).as_millis() as u64;
        match raw {
            Raw::Move { x, y, mods, held } => {
                let o = self.detector.mouse_move(x, y, mods, held, scope);
                out.extend(self.convert(o, now));
            }
            Raw::Down { button, x, y, mods } => {
                let o = self.detector.mouse_down(button, x, y, mods, now_ms, scope);
                out.extend(self.convert(o, now));
            }
            Raw::Up { x, y } => {
                self.pending_hold = None;
                let o = self.detector.mouse_up(x, y);
                out.extend(self.convert(o, now));
            }
            Raw::Key { vk, down, mods } => {
                if !down {
                    self.down_keys.remove(&vk);
                    return out;
                }
                // Auto-repeat: a key already down. Neither the flip key nor a chord fires again for it.
                let repeat = !self.down_keys.insert(vk);
                if repeat {
                    return out;
                }
                let Some(name) = keys::key_name(vk) else { return out };
                if watch_flip && self.flip_key.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(&name)) {
                    out.push(InputEvent::Flip);
                }
                let o = self.detector.key_down(&name, mods, scope);
                out.extend(self.convert(o, now));
            }
            Raw::Wheel => {
                if watch_flip && self.flip_with_wheel {
                    let same_turn = self.last_wheel.is_some_and(|l| now.saturating_duration_since(l) < WHEEL_QUIET);
                    self.last_wheel = Some(now);
                    if !same_turn {
                        out.push(InputEvent::Flip);
                    }
                }
            }
        }
        out
    }

    fn convert(&mut self, outputs: Vec<Output>, now: Instant) -> Vec<InputEvent> {
        let mut events = Vec::new();
        for o in outputs {
            match o {
                Output::DragStarted { x, y } => events.push(InputEvent::DragStarted { x, y }),
                Output::DragEnded { .. } => events.push(InputEvent::DragEnded),
                Output::Selection { x, y } => events.push(InputEvent::SelectionGesture { x, y }),
                Output::HoldTimer { token, ms } => {
                    self.pending_hold = Some((token, now + Duration::from_millis(ms as u64)));
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keys::{ALT, CTRL};
    use serde_json::json;

    fn yes(_: &Trigger, _: i32, _: i32) -> bool {
        true
    }
    const VK_TAB: u32 = 0x09;
    const VK_SPACE: u32 = 0x20;

    fn engine(settings: Value) -> (Engine, Instant) {
        let t0 = Instant::now();
        let mut e = Engine::new(4, t0);
        e.configure(&Config::from_settings(&settings));
        (e, t0)
    }
    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn config_reads_the_local_settings() {
        let c = Config::from_settings(&json!({
            "paused": true,
            "donut": {"flipKey": "Tab", "flipWithWheel": false},
            "excludedProcesses": ["a.exe", "b"],
            "triggers": [{"kind":"KeyChord","modifiers":"Ctrl, Alt","button":"None","key":"Space","scope":"FileManager"}]
        }));
        assert!(c.paused);
        assert_eq!(c.flip_key.as_deref(), Some("Tab"));
        assert!(!c.flip_with_wheel);
        assert_eq!(c.excluded, vec!["a.exe".to_string(), "b".to_string()]);
        assert_eq!(c.triggers.len(), 1);
    }

    #[test]
    fn config_defaults_when_settings_are_empty() {
        let c = Config::from_settings(&json!({}));
        assert!(!c.paused);
        assert_eq!(c.flip_key.as_deref(), Some("Tab"));
        assert!(c.flip_with_wheel);
        assert_eq!(c.triggers, Trigger::defaults());
    }

    #[test]
    fn flip_key_falls_back_to_tab_when_a_chord_took_it() {
        let c = Config::from_settings(&json!({
            "donut": {"flipKey": "Q"},
            "triggers": [{"kind":"KeyChord","modifiers":"Ctrl","key":"q"}]
        }));
        assert_eq!(c.flip_key.as_deref(), Some("Tab"));
        let cleared = Config::from_settings(&json!({"donut": {"flipKey": ""}}));
        assert_eq!(cleared.flip_key, None);
    }

    #[test]
    fn drag_gesture_posts_started_and_ended() {
        let (mut e, t0) = engine(json!({}));
        let mut s = yes;
        assert!(e.handle(Raw::Down { button: Button::Left, x: 0, y: 0, mods: CTRL }, t0, false, &mut s).is_empty());
        assert_eq!(
            e.handle(Raw::Move { x: 10, y: 0, mods: CTRL, held: Button::Left }, ms(t0, 5), false, &mut s),
            vec![InputEvent::DragStarted { x: 10, y: 0 }]
        );
        assert_eq!(e.handle(Raw::Up { x: 10, y: 0 }, ms(t0, 50), false, &mut s), vec![InputEvent::DragEnded]);
    }

    #[test]
    fn hold_timer_fires_from_tick_at_the_press_position() {
        let (mut e, t0) = engine(json!({"triggers":[{"kind":"Hold","modifiers":"Ctrl","button":"Left","holdMilliseconds":400}]}));
        let mut s = yes;
        e.handle(Raw::Down { button: Button::Left, x: 7, y: 8, mods: CTRL }, t0, false, &mut s);
        assert_eq!(e.next_deadline(), Some(ms(t0, 400)));
        assert!(e.tick(ms(t0, 399)).is_empty());
        assert_eq!(e.tick(ms(t0, 400)), vec![InputEvent::SelectionGesture { x: 7, y: 8 }]);
        assert_eq!(e.next_deadline(), None);
    }

    #[test]
    fn a_hold_that_is_due_fires_before_the_event_that_arrived_late() {
        let (mut e, t0) = engine(json!({"triggers":[{"kind":"Hold","modifiers":"Ctrl","button":"Left","holdMilliseconds":400}]}));
        let mut s = yes;
        e.handle(Raw::Down { button: Button::Left, x: 7, y: 8, mods: CTRL }, t0, false, &mut s);
        let out = e.handle(Raw::Up { x: 7, y: 8 }, ms(t0, 450), false, &mut s);
        assert_eq!(out, vec![InputEvent::SelectionGesture { x: 7, y: 8 }]);
    }

    #[test]
    fn release_cancels_the_hold_timer() {
        let (mut e, t0) = engine(json!({"triggers":[{"kind":"Hold","modifiers":"Ctrl","button":"Left","holdMilliseconds":400}]}));
        let mut s = yes;
        e.handle(Raw::Down { button: Button::Left, x: 7, y: 8, mods: CTRL }, t0, false, &mut s);
        assert!(e.handle(Raw::Up { x: 7, y: 8 }, ms(t0, 100), false, &mut s).is_empty());
        assert_eq!(e.next_deadline(), None);
        assert!(e.tick(ms(t0, 1000)).is_empty());
    }

    #[test]
    fn key_chord_fires_once_per_press_not_per_auto_repeat() {
        let (mut e, t0) = engine(json!({}));
        e.set_last_position(11, 22);
        let mut s = yes;
        let down = Raw::Key { vk: VK_SPACE, down: true, mods: CTRL | ALT };
        assert_eq!(e.handle(down, t0, false, &mut s), vec![InputEvent::SelectionGesture { x: 11, y: 22 }]);
        assert!(e.handle(down, ms(t0, 500), false, &mut s).is_empty());
        assert!(e.handle(down, ms(t0, 530), false, &mut s).is_empty());
        e.handle(Raw::Key { vk: VK_SPACE, down: false, mods: CTRL | ALT }, ms(t0, 600), false, &mut s);
        assert_eq!(e.handle(down, ms(t0, 900), false, &mut s).len(), 1);
    }

    #[test]
    fn flip_key_reports_only_while_watched_and_filters_auto_repeat() {
        let (mut e, t0) = engine(json!({}));
        let mut s = yes;
        let tab = Raw::Key { vk: VK_TAB, down: true, mods: CTRL };
        // not watching: nothing (but the key's state is followed)
        assert!(e.handle(tab, t0, false, &mut s).is_empty());
        e.handle(Raw::Key { vk: VK_TAB, down: false, mods: CTRL }, ms(t0, 10), false, &mut s);
        // watching
        assert_eq!(e.handle(tab, ms(t0, 20), true, &mut s), vec![InputEvent::Flip]);
        assert!(e.handle(tab, ms(t0, 60), true, &mut s).is_empty()); // auto-repeat
        e.handle(Raw::Key { vk: VK_TAB, down: false, mods: CTRL }, ms(t0, 80), true, &mut s);
        assert_eq!(e.handle(tab, ms(t0, 100), true, &mut s), vec![InputEvent::Flip]);
    }

    #[test]
    fn a_flip_key_held_down_before_watching_started_does_not_flip_when_repeating() {
        let (mut e, t0) = engine(json!({}));
        let mut s = yes;
        let tab = Raw::Key { vk: VK_TAB, down: true, mods: 0 };
        e.handle(tab, t0, false, &mut s);
        assert!(e.handle(tab, ms(t0, 30), true, &mut s).is_empty());
    }

    #[test]
    fn flip_key_ignores_modifiers_and_other_keys() {
        let (mut e, t0) = engine(json!({"donut": {"flipKey": "Q"}}));
        let mut s = yes;
        assert!(e.handle(Raw::Key { vk: VK_TAB, down: true, mods: 0 }, t0, true, &mut s).is_empty());
        assert_eq!(e.handle(Raw::Key { vk: 0x51, down: true, mods: CTRL | ALT }, ms(t0, 5), true, &mut s), vec![InputEvent::Flip]);
    }

    #[test]
    fn no_flip_key_means_no_flip_event() {
        let (mut e, t0) = engine(json!({"donut": {"flipKey": ""}}));
        let mut s = yes;
        assert!(e.handle(Raw::Key { vk: VK_TAB, down: true, mods: 0 }, t0, true, &mut s).is_empty());
    }

    #[test]
    fn wheel_notches_within_quiet_time_are_one_flip() {
        let (mut e, t0) = engine(json!({}));
        let mut s = yes;
        assert_eq!(e.handle(Raw::Wheel, t0, true, &mut s), vec![InputEvent::Flip]);
        assert!(e.handle(Raw::Wheel, ms(t0, 100), true, &mut s).is_empty());
        assert!(e.handle(Raw::Wheel, ms(t0, 250), true, &mut s).is_empty());
        // the quiet window slides: 299 after the last notch is still the same turn, 300 is a new one
        assert!(e.handle(Raw::Wheel, ms(t0, 549), true, &mut s).is_empty());
        assert_eq!(e.handle(Raw::Wheel, ms(t0, 849), true, &mut s), vec![InputEvent::Flip]);
    }

    #[test]
    fn wheel_is_ignored_unless_watched_and_enabled() {
        let (mut e, t0) = engine(json!({}));
        let mut s = yes;
        assert!(e.handle(Raw::Wheel, t0, false, &mut s).is_empty());
        let (mut off, t0) = engine(json!({"donut": {"flipWithWheel": false}}));
        assert!(off.handle(Raw::Wheel, t0, true, &mut s).is_empty());
    }

    #[test]
    fn paused_config_silences_gestures_but_not_flips() {
        let (mut e, t0) = engine(json!({"paused": true}));
        let mut s = yes;
        assert!(e.handle(Raw::Down { button: Button::Left, x: 0, y: 0, mods: CTRL }, t0, false, &mut s).is_empty());
        assert!(e.handle(Raw::Key { vk: VK_SPACE, down: true, mods: CTRL | ALT }, ms(t0, 1), false, &mut s).is_empty());
        assert_eq!(e.handle(Raw::Wheel, ms(t0, 2), true, &mut s), vec![InputEvent::Flip]);
    }

    #[test]
    fn scope_decides_per_event() {
        let (mut e, t0) = engine(json!({}));
        let mut deny = |_: &Trigger, _: i32, _: i32| false;
        assert!(e.handle(Raw::Down { button: Button::Left, x: 0, y: 0, mods: CTRL }, t0, false, &mut deny).is_empty());
        assert!(e
            .handle(Raw::Key { vk: VK_SPACE, down: true, mods: CTRL | ALT }, ms(t0, 1), false, &mut deny)
            .is_empty());
    }
}
