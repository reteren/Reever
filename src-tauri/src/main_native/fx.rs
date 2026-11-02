// Частицы у линзы главного окна: перенос createFx из skin/orb.js. Угли в покое, рой по
// наклонным орбитам при «слушаю», схлопывание к линзе при «выполняю», световые нити по
// хромовому кольцу. Рисуется прямо в кадр окна со сложением цветов (как 'lighter' у холста
// и mix-blend-mode: screen у #orbFx). Штрихи без скруглённых концов: построение круглых концов
// у сотен коротких отрезков в каждом кадре стоило больше всего остального кадра.

use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_POINT_2F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ELLIPSE, D2D1_PRIMITIVE_BLEND_ADD, D2D1_PRIMITIVE_BLEND_SOURCE_OVER,
};

use crate::native::RenderCx;

/// Режим роя — `mode` в shell.js (STATES[...].mode).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    None,
    Ember,
    Orbit,
    Collapse,
}

/// Входы кадра — как `render(st, dt)` в orb.js.
#[derive(Clone, Copy, Debug)]
pub struct FxState {
    pub mode: Mode,
    pub level: f32,
    pub energy: f32,
    pub threads: bool,
    pub burst: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Ember,
    Orbit,
}

#[derive(Clone, Debug)]
struct Part {
    kind: Kind,
    // угли
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    // орбиты
    a: f32,
    r: f32,
    tilt: f32,
    spin: f32,
    w: f32,
    vr: f32,
    prev: Option<(f32, f32)>,
    // общее
    life: f32,
    max: f32,
    size: f32,
    ph: f32,
    dying: bool,
    fade_out: f32,
}

struct Thread {
    a: f32,
    w: f32,
    r: f32,
    len: f32,
    tilt: f32,
    on: f32,
}

const ACCENT: (f32, f32, f32) = (39.0 / 255.0, 230.0 / 255.0, 170.0 / 255.0);
const TIP: (f32, f32, f32) = (225.0 / 255.0, 1.0, 246.0 / 255.0);
const CORE: (f32, f32, f32) = (215.0 / 255.0, 1.0, 242.0 / 255.0);

/// Линия или точка кадра (рисуются после расчёта всех частиц).
enum Mark {
    Line { c: (f32, f32, f32), a: f32, w: f32, p0: (f32, f32), p1: (f32, f32) },
    Dot { c: (f32, f32, f32), a: f32, p: (f32, f32), r: f32 },
}

pub struct Fx {
    center: (f32, f32),
    orb_r: f32,
    parts: Vec<Part>,
    threads: Vec<Thread>,
    seed: u64,
    marks: Vec<Mark>,
}

impl Fx {
    /// Рой вокруг линзы с центром `center` и радиусом `orb_r` (координаты скина).
    pub fn new(center: (f32, f32), orb_r: f32) -> Self {
        let threads = (0..4)
            .map(|i| {
                let f = i as f32;
                Thread {
                    a: f * 1.7,
                    w: if i % 2 == 1 { -1.0 } else { 1.0 } * (0.5 + f * 0.17),
                    r: orb_r + 26.0 + f * 9.0,
                    len: 0.9 + f * 0.25,
                    tilt: 0.25 + f * 0.22,
                    on: 0.0,
                }
            })
            .collect();
        Self { center, orb_r, parts: Vec::new(), threads, seed: 0x9E37_79B9_7F4A_7C15, marks: Vec::new() }
    }

    /// Есть что рисовать: частицы живы или нити не погасли.
    pub fn active(&self) -> bool {
        !self.parts.is_empty() || self.threads.iter().any(|t| t.on > 0.01)
    }

    fn rnd(&mut self, a: f32, b: f32) -> f32 {
        // xorshift64*: случайность только для вида, не для безопасности
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        let v = (self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32;
        a + v * (b - a)
    }

    fn spawn(&mut self, kind: Kind) -> Part {
        let (cx, cy) = self.center;
        let orb = self.orb_r;
        let mut p = Part {
            kind,
            x: 0.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            a: 0.0,
            r: 0.0,
            tilt: 0.0,
            spin: 0.0,
            w: 0.0,
            vr: 0.0,
            prev: None,
            life: 0.0,
            max: 0.0,
            size: 0.0,
            ph: 0.0,
            dying: false,
            fade_out: 1.0,
        };
        if kind == Kind::Ember {
            let a = self.rnd(std::f32::consts::PI * 0.15, std::f32::consts::PI * 0.85);
            let k = self.rnd(0.5, 1.05);
            p.x = cx + a.cos() * orb * k;
            p.y = cy + a.sin() * orb * 0.9;
            p.vx = self.rnd(-6.0, 6.0);
            p.vy = self.rnd(-34.0, -14.0);
            p.max = self.rnd(2.6, 5.5);
            p.size = self.rnd(0.8, 1.8);
            p.ph = self.rnd(0.0, 6.28);
        } else {
            p.a = self.rnd(0.0, std::f32::consts::TAU);
            p.r = self.rnd(orb + 12.0, orb + 95.0);
            p.tilt = self.rnd(-1.15, 1.15);
            p.spin = self.rnd(0.0, std::f32::consts::PI);
            let dir = if self.rnd(0.0, 1.0) < 0.82 { 1.0 } else { -1.0 };
            p.w = self.rnd(0.5, 1.5) * dir;
            p.max = self.rnd(2.5, 7.0);
            p.size = self.rnd(0.7, 2.6);
            p.ph = self.rnd(0.0, 6.28);
        }
        p
    }

    /// Шаг роя на `dt` секунд и раскладка кадра (рисует `draw`).
    pub fn step(&mut self, st: &FxState, dt: f32) {
        self.marks.clear();
        let want = match st.mode {
            Mode::Orbit => (100.0 + st.level * 100.0).round() as usize,
            Mode::Collapse => 150,
            Mode::Ember => 18,
            Mode::None => 0,
        };
        let kind = if st.mode == Mode::Ember { Kind::Ember } else { Kind::Orbit };
        for p in self.parts.iter_mut() {
            if p.kind != kind && !p.dying {
                p.dying = true;
            }
        }
        let mut alive = self.parts.iter().filter(|p| !p.dying).count();
        let mut k = 0;
        while k < 8 && alive < want {
            let p = self.spawn(kind);
            self.parts.push(p);
            k += 1;
            alive += 1;
        }
        if alive > want {
            let mut extra = alive - want;
            for p in self.parts.iter_mut() {
                if extra == 0 {
                    break;
                }
                if !p.dying {
                    p.dying = true;
                    extra -= 1;
                }
            }
        }

        let (cx, cy) = self.center;
        let orb = self.orb_r;
        let speed = 1.0 + st.level * 2.6 + if st.mode == Mode::Collapse { 1.5 } else { 0.0 };
        let mut i = self.parts.len();
        while i > 0 {
            i -= 1;
            let reborn = if st.mode == Mode::Collapse { Some(self.rnd(70.0, 110.0)) } else { None };
            let p = &mut self.parts[i];
            p.life += dt;
            let mut fade = (p.life / 0.35).min(1.0) * ((p.max - p.life) / 0.6).min(1.0);
            if p.dying {
                p.fade_out -= dt * 2.2;
                fade *= p.fade_out.max(0.0);
            }
            if p.life > p.max || (p.dying && p.fade_out <= 0.0) {
                self.parts.remove(i);
                continue;
            }
            if st.burst > 0.0 && p.kind == Kind::Orbit {
                p.vr += st.burst * 2600.0 * dt;
            }
            if p.kind == Kind::Ember {
                p.x += (p.vx + (p.life * 2.0 + p.ph).sin() * 8.0) * dt;
                p.y += p.vy * dt;
                self.marks.push(Mark::Dot { c: ACCENT, a: 0.55 * fade, p: (p.x, p.y), r: p.size });
                continue;
            }
            if st.mode == Mode::Collapse {
                p.r -= (55.0 + 3000.0 / (p.r - orb + 8.0).max(6.0)) * dt;
                if p.r < orb * 0.86 {
                    p.r = orb + reborn.unwrap_or(90.0);
                    p.prev = None;
                }
            }
            p.r += p.vr * dt;
            p.vr *= 0.04f32.powf(dt);
            let jitter = (p.life * 7.0 + p.ph).sin() * st.level * 22.0;
            let collapse_k = if st.mode == Mode::Collapse { (orb + 60.0) / p.r.max(orb * 0.8) } else { 1.0 };
            p.a += p.w * speed * collapse_k * dt;
            let rr = p.r + jitter;
            // орбита в наклонной плоскости: окружность вращается вокруг оси под углом spin
            let (ox, oy) = (p.a.cos() * rr, p.a.sin() * rr);
            let (yz, depth) = (oy * p.tilt.cos(), oy * p.tilt.sin());
            let x = cx + ox * p.spin.cos() - yz * p.spin.sin();
            let y = cy + ox * p.spin.sin() + yz * p.spin.cos();
            let behind = depth < 0.0 && ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() < orb * 0.98;
            if let (Some((px, py)), false) = (p.prev, behind) {
                let near = 0.55 + 0.45 * (depth / rr).clamp(-1.0, 1.0);
                let a = (fade * near * (0.5 + st.energy * 0.8)).min(1.0);
                let lw = p.size * near * (1.0 + st.level * 0.9);
                // мягкое гало — широкий бледный штрих, поверх — яркое ядро
                self.marks.push(Mark::Line { c: ACCENT, a: a * 0.22, w: lw * 4.5, p0: (px, py), p1: (x, y) });
                self.marks.push(Mark::Line { c: ACCENT, a, w: lw, p0: (px, py), p1: (x, y) });
                if p.size > 1.1 {
                    self.marks.push(Mark::Dot { c: CORE, a, p: (x, y), r: p.size * 0.75 });
                }
            }
            // след длиннее на скорости: старая точка держится чуть дольше
            let moved = p.prev.map_or(f32::MAX, |(px, py)| ((x - px).powi(2) + (y - py).powi(2)).sqrt());
            if p.prev.is_none() || moved > 2.5 + st.level * 8.0 {
                p.prev = Some((x, y));
            }
            if behind {
                p.prev = None;
            }
        }

        // нити: длинные световые дуги по хромовому кольцу
        for th in self.threads.iter_mut() {
            th.on += ((if st.threads { 1.0 } else { 0.0 }) - th.on) * (dt * 3.0).min(1.0);
            if th.on < 0.01 {
                continue;
            }
            th.a += th.w * dt * (1.0 + st.level * 2.0);
            let steps = 22;
            let span = th.len * (0.6 + 0.4 * th.on);
            let mut prev = (0.0, 0.0);
            for k in 0..=steps {
                let t = k as f32 / steps as f32;
                let a = th.a - span * t;
                let (ox, oy) = (a.cos() * th.r, a.sin() * th.r * th.tilt.cos());
                let pt = (cx + ox, cy + oy);
                if k > 0 {
                    let al = ((1.0 - t) * (1.0 - t) * th.on * (0.8 + st.level * 0.8)).min(1.0);
                    self.marks.push(Mark::Line { c: ACCENT, a: al * 0.25, w: 9.0 * (1.0 - t) + 1.0, p0: prev, p1: pt });
                    self.marks.push(Mark::Line { c: if k < 3 { TIP } else { ACCENT }, a: al, w: 2.6 * (1.0 - t) + 0.4, p0: prev, p1: pt });
                }
                prev = pt;
            }
        }
    }

    /// Нарисовать раскладку последнего шага сложением цветов.
    pub fn draw(&self, cx: &mut RenderCx<'_>) {
        if self.marks.is_empty() {
            return;
        }
        let dc = cx.dc();
        let brush = cx.brush(crate::native::Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 });
        unsafe {
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_ADD);
            for m in &self.marks {
                match m {
                    Mark::Line { c, a, w, p0, p1 } => {
                        if *a <= 0.004 {
                            continue;
                        }
                        brush.SetColor(&D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: *a });
                        dc.DrawLine(D2D_POINT_2F { x: p0.0, y: p0.1 }, D2D_POINT_2F { x: p1.0, y: p1.1 }, brush, *w, None);
                    }
                    Mark::Dot { c, a, p, r } => {
                        if *a <= 0.004 {
                            continue;
                        }
                        brush.SetColor(&D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: *a });
                        dc.FillEllipse(&D2D1_ELLIPSE { point: D2D_POINT_2F { x: p.0, y: p.1 }, radiusX: *r, radiusY: *r }, brush);
                    }
                }
            }
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_grows_to_the_wanted_size_and_dies_out() {
        let mut fx = Fx::new((310.0, 352.0), 100.0);
        let st = FxState { mode: Mode::Orbit, level: 0.0, energy: 1.0, threads: true, burst: 0.0 };
        for _ in 0..120 {
            fx.step(&st, 1.0 / 60.0);
        }
        assert!(fx.parts.len() >= 95, "{}", fx.parts.len());
        let off = FxState { mode: Mode::None, threads: false, ..st };
        for _ in 0..240 {
            fx.step(&off, 1.0 / 60.0);
        }
        assert!(!fx.active(), "everything fades out when the mode is off");
    }
}
