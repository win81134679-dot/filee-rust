//! The donut's spring (spec 01 §4.6): semi-implicit Euler, sub-stepped so a slow frame stays stable.

/// Damping at which a spring of stiffness 320 settles without overshooting (2·√320).
pub const CRITICAL: f64 = 36.0;
/// The default spring (a little bounce).
pub const STIFFNESS: f64 = 320.0;
pub const DAMPING: f64 = 20.0;
const MAX_STEP: f64 = 1.0 / 120.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spring {
    pub value: f64,
    pub velocity: f64,
}

impl Spring {
    pub fn at(value: f64) -> Spring {
        Spring { value, velocity: 0.0 }
    }

    /// Advances by `dt` seconds towards `target`; true while it is still moving.
    pub fn step(&mut self, target: f64, dt: f64, stiffness: f64, damping: f64) -> bool {
        if self.settled(target) {
            self.value = target;
            self.velocity = 0.0;
            return false;
        }
        let steps = ((dt / MAX_STEP).ceil() as i32).max(1);
        let h = dt / steps as f64;
        for _ in 0..steps {
            let force = stiffness * (target - self.value) - damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
        }
        if self.settled(target) {
            self.value = target;
            self.velocity = 0.0;
            return false;
        }
        true
    }

    fn settled(&self, target: f64) -> bool {
        (target - self.value).abs() < 0.001 && self.velocity.abs() < 0.01
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut Spring, target: f64, k: f64, c: f64) -> (usize, f64) {
        let (mut frames, mut peak) = (0, s.value);
        while s.step(target, 1.0 / 60.0, k, c) {
            frames += 1;
            peak = peak.max(s.value);
            assert!(frames < 600, "never settled");
        }
        (frames, peak)
    }

    #[test]
    fn underdamped_hover_overshoots_then_settles_on_target() {
        let mut s = Spring::default();
        let (frames, peak) = run(&mut s, 1.0, STIFFNESS, DAMPING);
        assert!(peak > 1.0, "k=320 c=20 should bounce, peak {peak}");
        assert!(frames > 10);
        assert_eq!(s.value, 1.0);
        assert_eq!(s.velocity, 0.0);
    }

    #[test]
    fn critical_damping_does_not_bounce() {
        let mut s = Spring::at(1.0);
        let (_, peak) = run(&mut s, 0.0, STIFFNESS, CRITICAL);
        assert!(peak <= 1.0);
        assert!(s.value.abs() < 1e-12);
        // Critically damped release never goes below zero (the accent would flash).
        let mut s = Spring::at(1.0);
        let mut low = 1.0f64;
        while s.step(0.0, 1.0 / 60.0, STIFFNESS, CRITICAL) {
            low = low.min(s.value);
        }
        assert!(low > -0.01, "undershoot {low}");
    }

    #[test]
    fn a_long_frame_is_substepped_and_stays_finite() {
        let mut s = Spring::default();
        assert!(s.step(1.0, 0.05, STIFFNESS, DAMPING));
        assert!(s.value.is_finite() && s.value > 0.0 && s.value < 2.0);
    }

    #[test]
    fn already_at_rest_reports_inactive() {
        let mut s = Spring::at(1.0);
        assert!(!s.step(1.0, 1.0 / 60.0, STIFFNESS, DAMPING));
    }

    #[test]
    fn centre_pulse_kick_decays() {
        // `PulseCenter` gives the spring an initial velocity of 6 towards a target of 0 (k=260, c=12).
        let mut s = Spring { value: 0.0, velocity: 6.0 };
        let (frames, peak) = run(&mut s, 0.0, 260.0, 12.0);
        assert!(peak > 0.1 && frames > 5);
    }
}
