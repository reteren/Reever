//! Переходы CSS (transition): кривая cubic-bezier и значение, плавно идущее к цели.

use std::time::Instant;

/// cubic-bezier(x1, y1, x2, y2), как в CSS.
#[derive(Clone, Copy, Debug)]
pub struct Bez(pub f32, pub f32, pub f32, pub f32);

pub const EASE: Bez = Bez(0.25, 0.1, 0.25, 1.0);
pub const EASE_IN: Bez = Bez(0.42, 0.0, 1.0, 1.0);
pub const EASE_OUT: Bez = Bez(0.0, 0.0, 0.58, 1.0);
/// --e-out
pub const E_OUT: Bez = Bez(0.16, 1.0, 0.3, 1.0);
/// --e-snap
pub const E_SNAP: Bez = Bez(0.22, 1.0, 0.36, 1.0);

impl Bez {
    pub fn at(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let Bez(x1, y1, x2, y2) = self;
        let curve = |t: f32, a: f32, b: f32| 3.0 * (1.0 - t) * (1.0 - t) * t * a + 3.0 * (1.0 - t) * t * t * b + t * t * t;
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if curve(mid, x1, x2) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        curve((lo + hi) / 2.0, y1, y2)
    }
}

/// Значение с переходом: `set` начинает переход от текущего значения к новой цели.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: f32,
    ease: Bez,
}

impl Tween {
    pub fn new(v: f32) -> Self {
        Self { from: v, to: v, start: Instant::now(), dur: 0.0, ease: EASE }
    }

    pub fn value(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        if self.dur <= 0.0 || t >= self.dur {
            return self.to;
        }
        self.from + (self.to - self.from) * self.ease.at(t / self.dur)
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    /// Новая цель; та же цель — ничего не меняется.
    pub fn set(&mut self, now: Instant, to: f32, dur: f32, ease: Bez) {
        if (self.to - to).abs() < f32::EPSILON {
            return;
        }
        *self = Self { from: self.value(now), to, start: now, dur, ease };
    }

    /// Сразу, без перехода.
    pub fn jump(&mut self, v: f32) {
        *self = Self::new(v);
    }

    pub fn running(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32() < self.dur && self.from != self.to
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bezier_endpoints_and_linear() {
        assert!((EASE.at(0.0)).abs() < 1e-4 && (EASE.at(1.0) - 1.0).abs() < 1e-4);
        let lin = Bez(0.0, 0.0, 1.0, 1.0);
        assert!((lin.at(0.3) - 0.3).abs() < 1e-3);
    }

    #[test]
    fn tween_reaches_target_and_restarts_from_current() {
        let t0 = Instant::now();
        let mut t = Tween::new(0.0);
        t.set(t0, 1.0, 0.2, Bez(0.0, 0.0, 1.0, 1.0));
        assert!((t.value(t0 + Duration::from_millis(100)) - 0.5).abs() < 0.02);
        t.set(t0 + Duration::from_millis(100), 0.0, 0.2, Bez(0.0, 0.0, 1.0, 1.0));
        assert!((t.value(t0 + Duration::from_millis(100)) - 0.5).abs() < 0.02);
        assert_eq!(t.value(t0 + Duration::from_secs(1)), 0.0);
    }
}
