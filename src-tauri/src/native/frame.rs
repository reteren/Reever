// Таймер кадров нативных окон: когда рисовать следующий кадр.
//
// Перерисовка идёт только по необходимости: окно «грязное» (что-то поменялось) или анимируется.
// Частота кадров — настройка «Плавность», те же значения, что у frontend/skin/fps.js:
// 0 — как у монитора, 60 или 30 — не чаще. Кадры ставятся на сетку (следующий срок = прошлый
// срок + промежуток), а не «через промежуток после фактического кадра»: таймер просыпается
// с опозданием в доли миллисекунды, и без сетки 60 кадров превращались бы в 58–59.
// Пауза (окно таскают) останавливает анимации; явная перерисовка при этом всё равно идёт.

use std::time::{Duration, Instant};

/// Допустимые значения лимита кадров, как в fps.js.
pub const FPS_LIMITS: [u32; 3] = [0, 60, 30];

/// Частота монитора, если её не удалось узнать у DWM.
const FALLBACK_REFRESH_HZ: f64 = 60.0;

#[derive(Debug, Clone)]
pub struct FrameClock {
    limit: u32,
    refresh_hz: f64,
    /// Срок следующего кадра на сетке; None — сетка не начата (давно не рисовали).
    next_due: Option<Instant>,
    paused_until: Option<Instant>,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameClock {
    pub fn new() -> Self {
        Self { limit: 0, refresh_hz: FALLBACK_REFRESH_HZ, next_due: None, paused_until: None }
    }

    /// Лимит кадров: 0 / 60 / 30; любое другое значение — 0 (как read() в fps.js).
    pub fn set_limit(&mut self, limit: u32) {
        self.limit = if FPS_LIMITS.contains(&limit) { limit } else { 0 };
        self.next_due = None;
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Частота монитора (для лимита 0). Нелепые значения игнорируются.
    pub fn set_refresh_hz(&mut self, hz: f64) {
        if hz.is_finite() && (20.0..=1000.0).contains(&hz) {
            self.refresh_hz = hz;
        }
    }

    pub fn refresh_hz(&self) -> f64 {
        self.refresh_hz
    }

    /// Промежуток между кадрами.
    pub fn interval(&self) -> Duration {
        let fps = if self.limit == 0 { self.refresh_hz } else { self.limit as f64 };
        Duration::from_secs_f64(1.0 / fps)
    }

    /// Остановить анимации на `dur` от `now` (продлевает, но не укорачивает уже идущую паузу).
    pub fn pause(&mut self, now: Instant, dur: Duration) {
        let until = now + dur;
        if self.paused_until.map_or(true, |p| p < until) {
            self.paused_until = Some(until);
        }
    }

    pub fn is_paused(&self, now: Instant) -> bool {
        self.paused_until.is_some_and(|p| now < p)
    }

    pub fn paused_until(&self) -> Option<Instant> {
        self.paused_until
    }

    /// Пора ли рисовать кадр сейчас (если есть что рисовать).
    pub fn due(&self, now: Instant) -> bool {
        self.next_due.map_or(true, |d| now >= d)
    }

    /// Когда можно рисовать следующий кадр; None — хоть сейчас.
    pub fn next_due(&self) -> Option<Instant> {
        self.next_due
    }

    /// Кадр нарисован в момент `now`: следующий срок на сетке. Если отстали больше чем на
    /// промежуток (долго не рисовали или кадр был тяжёлым), сетка начинается заново от `now` —
    /// иначе пошла бы очередь кадров подряд, «догоняющих» пропущенное.
    pub fn frame_done(&mut self, now: Instant) {
        let step = self.interval();
        let next = match self.next_due {
            Some(d) if now < d + step => d + step,
            _ => now + step,
        };
        self.next_due = Some(next);
    }

    /// Рисовать нечего: сетка сбрасывается, следующий кадр после простоя рисуется сразу.
    pub fn idle(&mut self, now: Instant) {
        if self.next_due.is_some_and(|d| now >= d) {
            self.next_due = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_accepts_only_known_values() {
        let mut c = FrameClock::new();
        c.set_limit(30);
        assert_eq!(c.limit(), 30);
        c.set_limit(45);
        assert_eq!(c.limit(), 0);
        c.set_limit(60);
        assert_eq!(c.interval(), Duration::from_secs_f64(1.0 / 60.0));
    }

    #[test]
    fn unlimited_follows_monitor_refresh() {
        let mut c = FrameClock::new();
        c.set_refresh_hz(144.0);
        assert_eq!(c.interval(), Duration::from_secs_f64(1.0 / 144.0));
        c.set_refresh_hz(0.0); // нелепое значение — остаётся прежнее
        assert_eq!(c.refresh_hz(), 144.0);
    }

    #[test]
    fn frames_stay_on_grid_despite_late_wakeups() {
        let mut c = FrameClock::new();
        c.set_limit(30);
        let t0 = Instant::now();
        let step = c.interval();
        c.frame_done(t0);
        // таймер проснулся на 2 мс позже — следующий срок всё равно t0 + 2 шага
        let late = t0 + step + Duration::from_millis(2);
        assert!(c.due(late));
        c.frame_done(late);
        assert_eq!(c.next_due(), Some(t0 + step * 2));
    }

    #[test]
    fn grid_restarts_after_long_gap() {
        let mut c = FrameClock::new();
        c.set_limit(60);
        let t0 = Instant::now();
        c.frame_done(t0);
        let later = t0 + Duration::from_secs(1);
        c.frame_done(later);
        assert_eq!(c.next_due(), Some(later + c.interval()));
    }

    #[test]
    fn not_due_before_deadline() {
        let mut c = FrameClock::new();
        c.set_limit(30);
        let t0 = Instant::now();
        c.frame_done(t0);
        assert!(!c.due(t0 + Duration::from_millis(10)));
        assert!(c.due(t0 + Duration::from_millis(34)));
    }

    #[test]
    fn idle_resets_grid() {
        let mut c = FrameClock::new();
        let t0 = Instant::now();
        c.frame_done(t0);
        c.idle(t0 + Duration::from_secs(1));
        assert_eq!(c.next_due(), None);
        assert!(c.due(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn pause_extends_but_never_shortens() {
        let mut c = FrameClock::new();
        let t0 = Instant::now();
        c.pause(t0, Duration::from_millis(500));
        c.pause(t0, Duration::from_millis(100));
        assert!(c.is_paused(t0 + Duration::from_millis(300)));
        assert!(!c.is_paused(t0 + Duration::from_millis(501)));
    }
}
