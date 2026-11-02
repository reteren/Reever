// Рисовальщик нативного маскота — построчный перенос frontend/skin/mascot.js (create) и
// окна frontend/mascot.html: те же наборы кадров и их темп, те же пружины состояний, свечение
// по силуэту, жидкая линза в глазу, частицы портала, хват за голову. Числа и порядок действий
// намеренно совпадают с JS — при правке одного правьте и другое.
//
// Отличия от веба, вынужденные платформой:
// - свечение: вместо blur() на уменьшенных в 2 и 4 раза холстах — эффект Direct2D Gaussian
//   blur с тем же радиусом (стандартное отклонение 10 и 3 базовых пикселя), на полном размере;
// - кадры для попиксельной проверки попадания — альфа, уменьшенная до 96×128 (в вебе читался
//   пиксель полного кадра 384×512);
// - курсор «рука» над головой и «перемещение» при хвате (у Windows нет курсоров grab/grabbing).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D_MATRIX_5X4_F, D2D_POINT_2F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1ColorMatrix, CLSID_D2D1GaussianBlur, CLSID_D2D1Opacity, ID2D1Bitmap1, ID2D1Effect, ID2D1StrokeStyle,
    D2D1_CAP_STYLE_ROUND, D2D1_ELLIPSE, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_PRIMITIVE_BLEND_ADD, D2D1_PRIMITIVE_BLEND_SOURCE_OVER, D2D1_PROPERTY_CACHED, D2D1_PROPERTY_TYPE_BOOL,
    D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_MATRIX_5X4, D2D1_STROKE_STYLE_PROPERTIES,
};

use super::lens::{Lens, LensParams};
use crate::native::{
    set_cursor, AlphaMask, Color, Cursor, GpuCx, ImageId, MouseButton, MouseEvent, MouseKind, RenderCx,
    WindowHandler,
};

// ── mascot.js ─────────────────────────────────────────────────────────
const FRAMES: usize = 84;
const LOOP_FROM: usize = 35;
const IDLE_FRAMES: usize = 96;
const PORTAL_FRAMES: usize = 58;
const FPS: f32 = 24.0;
const PORTAL_FPS: f32 = 48.0;
const RELEASE_FROM: usize = 19;
const GRAB_SPEED: f32 = 1.5;
const EYE_INSET: f32 = 0.97;
const GLOW: [u8; 3] = [70, 205, 165];
const ACC: [u8; 3] = [39, 230, 170];
// ── mascot.html ───────────────────────────────────────────────────────
const SPR_W: f32 = 120.0;
const SPR_H: f32 = 160.0;
const PAD: f32 = 22.0;
const FLY_W: f32 = 328.0;
const FLY_H: f32 = 408.0;
pub const HEAD_BOTTOM: f32 = 0.58;
/// Размер альфы кадра для проверки попадания (четверть 384×512).
pub const MASK_W: u32 = 96;
pub const MASK_H: u32 = 128;
/// Пиксель «непрозрачен» для мыши (как `data[3] > 40` в mascot.js).
pub const ALPHA_HIT: u8 = 40;

#[derive(Clone, Copy)]
struct Targets {
    fill: f32,
    energy: f32,
    wave: f32,
    swirl: f32,
    dark: f32,
    glitch: f32,
    glow: f32,
}

fn targets(name: &str) -> Option<Targets> {
    let t = |fill, energy, wave, swirl, dark, glitch, glow| Targets { fill, energy, wave, swirl, dark, glitch, glow };
    Some(match name {
        "idle" => t(0.17, 0.55, 0.14, 0.0, 0.0, 0.0, 0.45),
        "listening" => t(0.72, 1.0, 0.32, 0.0, 0.0, 0.0, 0.85),
        "busy" => t(0.9, 1.12, 0.22, 1.0, 0.0, 0.0, 1.0),
        "loading" => t(0.3, 0.6, 0.1, 0.3, 0.0, 0.0, 0.4),
        "paused" => t(0.0, 0.12, 0.0, 0.0, 1.0, 0.0, 0.12),
        "error" => t(0.22, 0.5, 0.1, 0.0, 0.3, 1.0, 0.35),
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct Spring {
    v: f32,
    t: f32,
    vel: f32,
    k: f32,
    c: f32,
}

impl Spring {
    const fn new(k: f32, c: f32) -> Self {
        Self { v: 0.0, t: 0.0, vel: 0.0, k, c }
    }

    fn step(&mut self, dt: f32) {
        let a = self.k * (self.t - self.v) - self.c * self.vel;
        self.vel += a * dt;
        self.v += self.vel * dt;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Rest,
    Grab,
    Release,
    PortalIn,
    PortalOut,
    Gone,
}

/// Кадры и разметка из anim.json.
pub struct Assets {
    pub grab: Vec<ImageId>,
    pub idle: Vec<ImageId>,
    pub portal_in: Vec<ImageId>,
    pub portal_gate: Vec<ImageId>,
    pub eye_grab: Vec<[f32; 7]>,
    pub eye_idle: Vec<[f32; 7]>,
    pub eye_portal_in: Vec<[f32; 7]>,
    pub portal_at_in: Vec<[f32; 3]>,
    pub portal_at_out: Vec<[f32; 3]>,
}

/// Сообщения окну — события, которые веб-маскот получал от Tauri.
#[derive(Debug, Clone)]
pub enum Msg {
    /// mascot://enter (true — только что показан)
    Enter(bool),
    /// mascot://leave
    Leave,
    /// mascot://hidden
    Hidden,
    /// mascot://drop
    Drop,
    /// reever://state
    State(String),
    /// reever://wake
    Wake,
    /// reever://wake-rejected
    WakeRejected,
    /// reever://level (RMS)
    Level(f32),
    /// reever://heard (непустой текст)
    Heard,
    /// размер или запас окна сменились (mascot_layout)
    Relayout,
}

#[derive(Clone, Copy, PartialEq)]
enum SparkKind {
    Orbit,
    Burst,
    Suck,
}

struct Spark {
    kind: SparkKind,
    a: f32,
    rr: f32,
    w: f32,
    vr: f32,
    life: f32,
    max: f32,
    size: f32,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    // центр и радиус портала (у orbit/suck)
    c: Option<(f32, f32, f32)>,
    // прошлая и текущая точка — штрих рисуется между ними (в первый кадр штриха нет)
    started: bool,
    prev: Option<(f32, f32)>,
    cur: (f32, f32),
    fade: f32,
}

/// Узнать о кадре: что рисуется и где (для проверки попадания из потока слежения за курсором).
#[derive(Clone)]
pub struct HitFrame {
    /// прямоугольник спрайта в пикселях окна: x, y, w, h
    pub rect_px: [f32; 4],
    pub mask: Option<Arc<AlphaMask>>,
}

/// Эффекты свечения на устройстве.
struct Glow {
    generation: u64,
    tint: ID2D1Effect,
    wide: ID2D1Effect,
    near: ID2D1Effect,
    wide_op: ID2D1Effect,
    near_op: ID2D1Effect,
    input: Option<ID2D1Bitmap1>,
    sigma: (f32, f32),
    stroke: windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle,
}

pub struct MascotView {
    a: Assets,
    hwnd: isize,
    // раскладка G (базовые пиксели маскота при 100 %; k — базовые пиксели → DIP)
    gw: f32,
    gh: f32,
    spr: [f32; 4],
    fly: [f32; 4],
    k: f32,
    laid_for: (f32, f32),
    // пружины
    fill: Spring,
    energy: Spring,
    wave: Spring,
    swirl: Spring,
    dark: Spring,
    glitch: Spring,
    glow: Spring,
    state: String,
    raw: String,
    woke: bool,
    woke_until: Option<Instant>,
    level: f32,
    level_raw: f32,
    flash: f32,
    mode: Mode,
    frame: usize,
    idle_frame: usize,
    acc: f32,
    t0: Instant,
    time: f32,
    sparks: Vec<Spark>,
    spawn_acc: [f32; 3],
    popped: bool,
    held: bool,
    entered: bool,
    on_head_cursor: bool,
    lens: Option<Lens>,
    lens_params: LensParams,
    glow_fx: Option<Glow>,
    glow_generation: u64,
    masks: HashMap<ImageId, Arc<AlphaMask>>,
    rng: u64,
}

impl MascotView {
    pub fn new(assets: Assets, state: &str) -> Self {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        let mut v = Self {
            a: assets,
            hwnd: 0,
            gw: 164.0,
            gh: 204.0,
            spr: [22.0, 22.0, SPR_W, SPR_H],
            fly: [-82.0, -102.0, FLY_W, FLY_H],
            k: 1.0,
            laid_for: (0.0, 0.0),
            fill: Spring::new(46.0, 7.5),
            energy: Spring::new(30.0, 10.0),
            wave: Spring::new(40.0, 12.0),
            swirl: Spring::new(10.0, 6.0),
            dark: Spring::new(18.0, 9.0),
            glitch: Spring::new(60.0, 14.0),
            glow: Spring::new(14.0, 7.0),
            state: "idle".into(),
            raw: "idle".into(),
            woke: false,
            woke_until: None,
            level: 0.0,
            level_raw: 0.0,
            flash: 0.0,
            mode: Mode::Gone,
            frame: 0,
            idle_frame: 0,
            acc: 0.0,
            t0: Instant::now(),
            time: 0.0,
            sparks: Vec::new(),
            spawn_acc: [0.0; 3],
            popped: false,
            held: false,
            entered: false,
            on_head_cursor: false,
            lens: None,
            lens_params: LensParams::default(),
            glow_fx: None,
            glow_generation: 0,
            masks: HashMap::new(),
            rng: seed,
        };
        v.set_state(state);
        v
    }

    // ── состояния (setState / setWoke) ──────────────────────────────
    fn set_woke(&mut self, on: bool) {
        self.woke = on;
        self.woke_until = on.then(|| Instant::now() + Duration::from_secs(60));
        let raw = self.raw.clone();
        self.set_state(&raw);
    }

    fn set_state(&mut self, name: &str) {
        self.raw = if targets(name).is_some() { name.to_string() } else { "idle".into() };
        if matches!(self.raw.as_str(), "listening" | "busy" | "paused") {
            self.woke = false;
            self.woke_until = None;
        }
        self.state = if self.woke && matches!(self.raw.as_str(), "idle" | "loading") { "listening".into() } else { self.raw.clone() };
        let t = targets(&self.state).expect("state");
        self.fill.t = t.fill;
        self.energy.t = t.energy;
        self.wave.t = t.wave;
        self.swirl.t = t.swirl;
        self.dark.t = t.dark;
        self.glitch.t = t.glitch;
        self.glow.t = t.glow;
    }

    fn check_woke_timer(&mut self) {
        if self.woke_until.is_some_and(|t| Instant::now() >= t) {
            self.set_woke(false);
        }
    }

    fn set_resting(&self, rest: bool) {
        crate::mascot::mascot_rest(rest);
    }

    // ── сообщения (mascot.html + слушатели mascot.js) ───────────────
    pub fn on_msg(&mut self, msg: Msg) {
        self.check_woke_timer();
        match msg {
            Msg::Enter(fresh) => {
                self.entered = true;
                if self.mode == Mode::PortalIn {
                    return;
                }
                if fresh || self.mode == Mode::Gone {
                    self.set_resting(false);
                    self.arrive();
                } else if self.mode == Mode::PortalOut {
                    self.present();
                    self.set_resting(true);
                }
            }
            Msg::Leave => {
                if !self.held && self.mode != Mode::Gone {
                    self.set_resting(false);
                    self.depart();
                }
            }
            Msg::Hidden => {
                self.entered = false;
                self.set_resting(false);
                self.absent();
                // события из разных потоков могли прийти не по порядку — сверка с Rust
                let (_, _, shown) = crate::mascot::mascot_layout();
                if shown && !self.entered && self.mode == Mode::Gone {
                    self.set_resting(false);
                    self.arrive();
                }
            }
            Msg::Drop => self.drop_held(),
            Msg::State(s) => self.set_state(&s),
            Msg::Wake => self.set_woke(true),
            Msg::WakeRejected => self.set_woke(false),
            Msg::Level(rms) => {
                self.level_raw = if matches!(self.state.as_str(), "listening" | "busy") {
                    ((20.0 * (rms + 1e-6).log10() + 54.0) / 40.0).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
            Msg::Heard => self.flash = 1.0,
            Msg::Relayout => self.laid_for = (0.0, 0.0),
        }
    }

    // прийти через портал / уйти в портал / сразу стоять / сразу исчезнуть
    fn arrive(&mut self) {
        self.mode = Mode::PortalIn;
        self.frame = 0;
        self.acc = 0.0;
    }

    fn depart(&mut self) {
        if self.mode == Mode::Gone {
            return;
        }
        self.mode = Mode::PortalOut;
        self.frame = 0;
        self.acc = 0.0;
    }

    fn present(&mut self) {
        if self.mode == Mode::Gone || self.in_portal() {
            self.mode = Mode::Rest;
            self.idle_frame = 0;
            self.frame = 0;
        }
    }

    fn absent(&mut self) {
        self.mode = Mode::Gone;
        self.frame = 0;
        self.sparks.clear();
    }

    fn grab(&mut self) {
        if self.mode != Mode::Grab {
            self.mode = Mode::Grab;
            if self.frame >= LOOP_FROM {
                self.frame = 0;
            }
        }
    }

    fn drop_held(&mut self) {
        if !self.held {
            return;
        }
        self.held = false;
        set_cursor(self.hwnd, Cursor::Arrow);
        self.mode = Mode::Release;
        self.frame = self.frame.min(RELEASE_FROM);
    }

    fn in_portal(&self) -> bool {
        matches!(self.mode, Mode::PortalIn | Mode::PortalOut)
    }

    /// Кадр сейчас: картинка, прямоугольник (базовые px) и купол глаза.
    fn current(&self) -> Option<(ImageId, [f32; 4], Option<[f32; 7]>)> {
        let a = &self.a;
        match self.mode {
            Mode::Gone => None,
            Mode::PortalIn => Some((a.portal_in[self.frame], self.fly, a.eye_portal_in.get(self.frame).copied())),
            Mode::Rest | Mode::PortalOut => Some((a.idle[self.idle_frame], self.spr, a.eye_idle.get(self.idle_frame).copied())),
            Mode::Grab | Mode::Release => Some((a.grab[self.frame], self.spr, a.eye_grab.get(self.frame).copied())),
        }
    }

    fn step_frame(&mut self) {
        match self.mode {
            Mode::Rest => self.idle_frame = (self.idle_frame + 1) % IDLE_FRAMES,
            Mode::Grab => self.frame = if self.frame + 1 >= FRAMES { LOOP_FROM } else { self.frame + 1 },
            Mode::Release => {
                self.frame = self.frame.saturating_sub(1);
                if self.frame == 0 {
                    self.mode = Mode::Rest;
                    self.idle_frame = 0;
                }
            }
            Mode::PortalIn | Mode::PortalOut => {
                if self.mode == Mode::PortalOut && self.frame % 2 == 1 {
                    self.idle_frame = (self.idle_frame + 1) % IDLE_FRAMES;
                }
                if self.frame + 1 < PORTAL_FRAMES {
                    self.frame += 1;
                } else if self.mode == Mode::PortalIn {
                    self.mode = Mode::Rest;
                    self.idle_frame = 0;
                    self.frame = 0;
                    self.set_resting(true); // onArrived
                } else {
                    self.mode = Mode::Gone;
                    self.frame = 0;
                }
            }
            Mode::Gone => {}
        }
    }

    // ── частицы портала ─────────────────────────────────────────────
    fn rnd(&mut self, a: f32, b: f32) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = (self.rng.wrapping_mul(0x2545F4914F6CDD1D) >> 40) as f32 / (1u64 << 24) as f32;
        a + r * (b - a)
    }

    fn portal_now(&self) -> Option<(f32, f32, f32)> {
        let list = match self.mode {
            Mode::PortalIn => &self.a.portal_at_in,
            Mode::PortalOut => &self.a.portal_at_out,
            _ => return None,
        };
        let p = list.get(self.frame)?;
        let [x, y, w, _h] = self.fly;
        Some((x + p[0] * w, y + p[1] * self.fly[3], p[2] * (w / 328.0)))
    }

    fn spawn(&mut self, kind: SparkKind, p: (f32, f32, f32)) {
        let a = self.rnd(0.0, std::f32::consts::TAU);
        let sc = self.fly[2] / 328.0;
        let mut s = Spark {
            kind,
            a,
            rr: 0.0,
            w: 0.0,
            vr: 0.0,
            life: 0.0,
            max: 0.0,
            size: 0.0,
            x: 0.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            c: None,
            started: false,
            prev: None,
            cur: (0.0, 0.0),
            fade: 0.0,
        };
        match kind {
            SparkKind::Orbit => {
                s.rr = self.rnd(0.92, 1.12);
                s.w = self.rnd(2.2, 4.2);
                s.max = self.rnd(0.35, 0.8);
                s.size = self.rnd(0.7, 1.8) * sc;
            }
            SparkKind::Suck => {
                s.rr = self.rnd(1.3, 2.3);
                s.w = self.rnd(2.5, 4.5);
                s.vr = self.rnd(1.6, 2.6);
                s.max = 1.4;
                s.size = self.rnd(0.7, 1.9) * sc;
            }
            SparkKind::Burst => {
                let sp = self.rnd(70.0, 230.0) * sc;
                let r0 = p.2 * self.rnd(0.0, 0.35);
                s.x = p.0 + a.cos() * r0;
                s.y = p.1 + a.sin() * r0;
                s.vx = a.cos() * sp;
                s.vy = a.sin() * sp;
                s.max = self.rnd(0.4, 0.9);
                s.size = self.rnd(0.8, 2.2) * sc;
            }
        }
        self.sparks.push(s);
    }

    fn portal_sparks(&mut self, dt: f32) {
        if !self.in_portal() {
            self.popped = false;
            return;
        }
        let Some(p) = self.portal_now() else { return };
        let t = self.frame as f32 / (PORTAL_FRAMES - 1) as f32;
        let mut rate = [if p.2 > 6.0 { 110.0 } else { 0.0 }, 0.0, 0.0];
        if self.mode == Mode::PortalIn && t > 0.14 && t < 0.55 {
            rate[1] = 70.0;
        }
        if self.mode == Mode::PortalOut && t > 0.05 && t < 0.85 {
            rate[2] = 140.0;
        }
        for (i, kind) in [SparkKind::Orbit, SparkKind::Burst, SparkKind::Suck].into_iter().enumerate() {
            self.spawn_acc[i] += rate[i] * dt;
            while self.spawn_acc[i] >= 1.0 {
                self.spawn_acc[i] -= 1.0;
                self.spawn(kind, p);
            }
        }
        let closing = if self.mode == Mode::PortalIn { t > 0.88 } else { t > 0.93 };
        if closing && !self.popped {
            self.popped = true;
            for _ in 0..26 {
                self.spawn(SparkKind::Burst, (p.0, p.1, 4.0));
            }
        }
        for s in &mut self.sparks {
            if s.kind != SparkKind::Burst {
                s.c = Some(p);
            }
        }
    }

    /// Движение искр (в mascot.js — внутри drawSparks).
    fn move_sparks(&mut self, dt: f32) {
        let mut i = self.sparks.len();
        while i > 0 {
            i -= 1;
            let s = &mut self.sparks[i];
            s.life += dt;
            let (x, y);
            if s.kind == SparkKind::Burst {
                s.vx *= 0.12f32.powf(dt);
                s.vy *= 0.12f32.powf(dt);
                x = s.x + s.vx * dt;
                y = s.y + s.vy * dt;
            } else {
                let Some((cx, cy, r)) = s.c else {
                    self.sparks.remove(i);
                    continue;
                };
                s.a += s.w * dt * if s.kind == SparkKind::Suck { 1.0 + 1.5 / s.rr.max(0.2) } else { 1.0 };
                if s.kind == SparkKind::Suck {
                    s.rr -= s.vr * dt;
                }
                x = cx + s.a.cos() * s.rr * r;
                y = cy + s.a.sin() * s.rr * r;
            }
            let dead = s.life > s.max
                || (s.kind == SparkKind::Suck && s.rr < 0.08)
                || (s.kind != SparkKind::Burst && s.c.is_some_and(|c| c.2 < 2.0));
            if dead {
                self.sparks.remove(i);
                continue;
            }
            s.fade = (s.life / 0.12).min(1.0) * ((s.max - s.life) / 0.25).min(1.0);
            s.prev = s.started.then_some(s.cur);
            s.started = true;
            s.cur = (x, y);
            if s.kind == SparkKind::Burst {
                s.x = x;
                s.y = y;
            }
        }
    }

    // ── раскладка (resize в mascot.html) ────────────────────────────
    fn relayout(&mut self, size_dip: (f32, f32)) {
        let (_, big, _) = crate::mascot::mascot_layout();
        let big = if big > 0.0 { big as f32 } else { 2.0 };
        let w = (SPR_W + 2.0 * PAD) * big;
        let h = (SPR_H + 2.0 * PAD) * big;
        self.gw = w;
        self.gh = h;
        self.k = if w > 0.0 { size_dip.0 / w } else { 1.0 };
        self.spr = [(w - SPR_W) / 2.0, (h - SPR_H) / 2.0, SPR_W, SPR_H];
        self.fly = [(w - FLY_W) / 2.0, (h - FLY_H) / 2.0, FLY_W, FLY_H];
        self.laid_for = size_dip;
    }

    /// Голова под точкой (DIP окна): выше линии HEAD_BOTTOM и непрозрачный пиксель кадра.
    fn on_head(&self, x: f32, y: f32) -> bool {
        if self.in_portal() || self.mode == Mode::Gone {
            return false;
        }
        let [_, sy, _, sh] = self.spr;
        if y < sy * self.k || y > (sy + sh * HEAD_BOTTOM) * self.k {
            return false;
        }
        self.opaque_at(x, y)
    }

    fn opaque_at(&self, x: f32, y: f32) -> bool {
        let Some((img, [rx, ry, rw, rh], _)) = self.current() else { return false };
        let Some(mask) = self.masks.get(&img) else { return false };
        let fx = (x / self.k - rx) / rw;
        let fy = (y / self.k - ry) / rh;
        mask.at(fx, fy) > ALPHA_HIT
    }

    fn glow_fx(&mut self, cx: &RenderCx<'_>, generation: u64) -> Option<&Glow> {
        if self.glow_fx.as_ref().is_some_and(|g| g.generation == generation) {
            return self.glow_fx.as_ref();
        }
        self.glow_fx = None;
        let dc = cx.dc();
        let made = unsafe {
            (|| -> windows::core::Result<Glow> {
                let tint = dc.CreateEffect(&CLSID_D2D1ColorMatrix)?;
                // цвет — GLOW, альфа — альфа кадра (source-in заливка силуэта в mascot.js)
                let m = D2D_MATRIX_5X4_F {
                    Anonymous: windows::Win32::Graphics::Direct2D::Common::D2D_MATRIX_5X4_F_0 {
                        Anonymous: windows::Win32::Graphics::Direct2D::Common::D2D_MATRIX_5X4_F_0_0 {
                            _11: 0.0, _12: 0.0, _13: 0.0, _14: 0.0,
                            _21: 0.0, _22: 0.0, _23: 0.0, _24: 0.0,
                            _31: 0.0, _32: 0.0, _33: 0.0, _34: 0.0,
                            _41: 0.0, _42: 0.0, _43: 0.0, _44: 1.0,
                            _51: GLOW[0] as f32 / 255.0, _52: GLOW[1] as f32 / 255.0, _53: GLOW[2] as f32 / 255.0, _54: 0.0,
                        },
                    },
                };
                set_prop(&tint, 0, D2D1_PROPERTY_TYPE_MATRIX_5X4.0, as_bytes(&m))?;
                set_prop(&tint, D2D1_PROPERTY_CACHED.0 as u32, D2D1_PROPERTY_TYPE_BOOL.0, as_bytes(&BOOL(1)))?;
                let tint_out = tint.GetOutput()?;
                let wide = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
                wide.SetInput(0, &tint_out, true);
                set_prop(&wide, D2D1_PROPERTY_CACHED.0 as u32, D2D1_PROPERTY_TYPE_BOOL.0, as_bytes(&BOOL(1)))?;
                let near = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
                near.SetInput(0, &tint_out, true);
                set_prop(&near, D2D1_PROPERTY_CACHED.0 as u32, D2D1_PROPERTY_TYPE_BOOL.0, as_bytes(&BOOL(1)))?;
                let wide_op = dc.CreateEffect(&CLSID_D2D1Opacity)?;
                wide_op.SetInput(0, &wide.GetOutput()?, true);
                let near_op = dc.CreateEffect(&CLSID_D2D1Opacity)?;
                near_op.SetInput(0, &near.GetOutput()?, true);
                let factory = dc.GetFactory()?;
                let stroke: ID2D1StrokeStyle = factory.CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        ..Default::default()
                    },
                    None,
                )?;
                Ok(Glow { generation, tint, wide, near, wide_op, near_op, input: None, sigma: (-1.0, -1.0), stroke })
            })()
        };
        match made {
            Ok(g) => self.glow_fx = Some(g),
            Err(e) => eprintln!("[reever] mascot glow: {e}"),
        }
        self.glow_fx.as_ref()
    }
}

fn as_bytes<T>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

unsafe fn set_prop(effect: &ID2D1Effect, index: u32, kind: i32, data: &[u8]) -> windows::core::Result<()> {
    effect.SetValue(index, windows::Win32::Graphics::Direct2D::D2D1_PROPERTY_TYPE(kind), data)
}

fn mat_translate(x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: x, M32: y }
}

fn mat_scale(s: f32) -> Matrix3x2 {
    Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 }
}

/// Поворот как у canvas rotate(θ): по часовой при оси y вниз.
fn mat_rotate(t: f32) -> Matrix3x2 {
    let (s, c) = t.sin_cos();
    Matrix3x2 { M11: c, M12: s, M21: -s, M22: c, M31: 0.0, M32: 0.0 }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl WindowHandler for MascotView {
    fn attached(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
    }

    fn animating(&self) -> bool {
        self.mode != Mode::Gone || !self.sparks.is_empty()
    }

    fn advance(&mut self, dt: Duration) {
        self.check_woke_timer();
        let dt = dt.as_secs_f32().min(0.05);
        self.acc += dt;
        self.time = self.t0.elapsed().as_secs_f32();
        let fps = if self.in_portal() {
            PORTAL_FPS
        } else if self.mode == Mode::Release {
            FPS * 1.6 * GRAB_SPEED
        } else if self.mode == Mode::Grab && self.frame < LOOP_FROM {
            FPS * GRAB_SPEED
        } else {
            FPS
        };
        let step = 1.0 / fps;
        while self.acc >= step {
            self.acc -= step;
            self.step_frame();
        }
        // уровень: быстрая атака, медленный спад; доли — за кадр 60 Гц
        let base: f32 = if self.level_raw > self.level { 0.45 } else { 0.88 };
        self.level += (self.level_raw - self.level) * (1.0 - base.powf(dt * 60.0));
        let t = targets(&self.state).expect("state");
        let lv = match self.state.as_str() {
            "listening" => self.level,
            "busy" => self.level * 0.4,
            _ => 0.0,
        };
        self.fill.t = t.fill + lv * 0.2 + if self.state == "loading" { (self.time * 2.2).sin() * 0.08 } else { 0.0 };
        self.wave.t = t.wave + lv * 2.4;
        self.energy.t = t.energy + lv * 0.5;
        self.glow.t = t.glow + lv * 0.9;
        for s in [&mut self.fill, &mut self.energy, &mut self.wave, &mut self.swirl, &mut self.dark, &mut self.glitch, &mut self.glow] {
            s.step(dt);
        }
        self.flash = (self.flash - dt * 2.6).max(0.0);
        self.portal_sparks(dt);
        self.move_sparks(dt);
    }

    fn mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Move => {
                if !self.held {
                    let on = self.on_head(ev.x, ev.y);
                    if on != self.on_head_cursor {
                        self.on_head_cursor = on;
                        set_cursor(self.hwnd, if on { Cursor::Hand } else { Cursor::Arrow });
                    }
                }
            }
            MouseKind::Down(MouseButton::Left) => {
                if !self.held && self.on_head(ev.x, ev.y) {
                    self.held = true;
                    set_cursor(self.hwnd, Cursor::Move);
                    self.grab();
                    crate::mascot::drag_native();
                    return true;
                }
            }
            MouseKind::Leave => {
                self.on_head_cursor = false;
            }
            _ => {}
        }
        false
    }

    fn prepare(&mut self, gpu: &GpuCx<'_>) {
        self.glow_generation = gpu.generation;
        // линза — только если глаз сейчас виден
        let Some((_, _, Some(e))) = self.current() else { return };
        if (e[6] - 0.15) / 0.3 <= 0.0 {
            return;
        }
        let size = (40.0 * self.k * gpu.scale.min(2.0)).round().max(2.0) as u32;
        if !self.lens.as_ref().is_some_and(|l| l.fits(gpu, size)) {
            self.lens = match Lens::new(gpu, size) {
                Ok(l) => l,
                Err(err) => {
                    eprintln!("[reever] mascot lens: {err}");
                    None
                }
            };
        }
        self.lens_params = LensParams {
            time: self.time,
            fill: self.fill.v.max(0.0),
            wave: self.wave.v,
            energy: self.energy.v.max(0.0),
            swirl: self.swirl.v,
            flash: self.flash,
            dark: self.dark.v.max(0.0),
            glitch: self.glitch.v.max(0.0),
        };
        if let Some(lens) = &self.lens {
            lens.render(gpu, &self.lens_params);
        }
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        if self.laid_for != cx.size {
            self.relayout(cx.size);
        }
        let k = self.k;
        let s = cx.scale;
        let dc = cx.dc().clone();
        let cur = self.current();

        // проверка попадания: что сейчас на экране и где (поток курсора в mascot_native)
        let hit = match (self.mode, cur) {
            (Mode::Rest | Mode::Grab | Mode::Release, Some((img, r, _))) => {
                let want = super::mask_wanted() || self.held;
                if want && !self.masks.contains_key(&img) {
                    if let Ok(m) = cx.images().alpha_mask(img, MASK_W, MASK_H) {
                        self.masks.insert(img, Arc::new(m));
                    }
                }
                Some(HitFrame {
                    rect_px: [r[0] * k * s, r[1] * k * s, r[2] * k * s, r[3] * k * s],
                    mask: self.masks.get(&img).cloned(),
                })
            }
            _ => None,
        };
        super::publish_hit(hit);

        let mut cur = cur;
        let mut pull: Option<(f32, f32, f32, f32)> = None; // x, y (DIP), масштаб, поворот
        unsafe {
            if self.mode == Mode::PortalOut {
                let gate = self.a.portal_gate[self.frame];
                let [x, y, w, h] = self.fly;
                let (pw, ph) = ((w * k * s).round() as u32, (h * k * s).round() as u32);
                if let Some(b) = cx.bitmap(gate, pw, ph) {
                    dc.DrawBitmap(&b, Some(&rect(x * k, y * k, w * k, h * k)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                }
                let t = self.frame as f32 / (PORTAL_FRAMES - 1) as f32;
                let e = ((t - 0.3) / 0.5).clamp(0.0, 1.0).powi(3);
                let p = self.portal_now();
                let px = p.map_or(self.gw / 2.0, |p| p.0) * k;
                let py = p.map_or(self.gh / 2.0, |p| p.1) * k;
                pull = Some((px, py, 1.0 - e, -e * std::f32::consts::PI * 4.0 / 3.0));
                if 1.0 - e < 0.01 {
                    cur = None;
                }
            }
            if let Some((img, [x, y, w, h], eye)) = cur {
                // перенос «к порталу»: translate(p) rotate scale translate(-p)
                let base = match pull {
                    Some((px, py, sc, rot)) => mat_translate(-px, -py) * mat_scale(sc) * mat_rotate(rot) * mat_translate(px, py),
                    None => Matrix3x2::identity(),
                };
                let (pw, ph) = ((w * k * s).round().max(1.0) as u32, (h * k * s).round().max(1.0) as u32);
                let bitmap = cx.bitmap(img, pw, ph);
                // свечение «дышит» и разгорается от голоса и принятой команды
                let g = (self.glow.v + self.flash * 0.8).max(0.0) * (0.85 + 0.15 * (self.time * 1.7).sin());
                if g > 0.01 {
                    if let Some(b) = &bitmap {
                        let gen = self.glow_generation;
                        if let Some(fx) = self.glow_fx(cx, gen) {
                            let sigma = (10.0 * k * s, 3.0 * k * s);
                            if fx.input.as_ref() != Some(b) {
                                fx.tint.SetInput(0, b, true);
                            }
                            if fx.sigma != sigma {
                                let _ = set_prop(&fx.wide, 0, D2D1_PROPERTY_TYPE_FLOAT.0, as_bytes(&sigma.0));
                                let _ = set_prop(&fx.near, 0, D2D1_PROPERTY_TYPE_FLOAT.0, as_bytes(&sigma.1));
                            }
                            let _ = set_prop(&fx.wide_op, 0, D2D1_PROPERTY_TYPE_FLOAT.0, as_bytes(&(0.75 * g).min(1.0)));
                            let _ = set_prop(&fx.near_op, 0, D2D1_PROPERTY_TYPE_FLOAT.0, as_bytes(&(0.35 * g).min(1.0)));
                            // эффекты — в пикселях: DPI контекста на время 96, битмап 1:1
                            let dip_to_px = mat_scale(s);
                            let m = mat_scale(1.0 / s) * mat_translate(x * k, y * k) * base * dip_to_px;
                            let dpi = s * 96.0;
                            dc.SetDpi(96.0, 96.0);
                            dc.SetTransform(&m);
                            for fx_out in [&fx.wide_op, &fx.near_op] {
                                if let Ok(out) = fx_out.GetOutput() {
                                    dc.DrawImage(&out, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                                }
                            }
                            dc.SetDpi(dpi, dpi);
                        }
                        if let Some(fx) = self.glow_fx.as_mut() {
                            fx.input = Some(b.clone());
                            fx.sigma = (10.0 * k * s, 3.0 * k * s);
                        }
                    }
                }
                dc.SetTransform(&base);
                if let Some(b) = &bitmap {
                    dc.DrawBitmap(b, Some(&rect(x * k, y * k, w * k, h * k)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                }
                // линза — по эллипсу купола; отвернувшийся глаз не светится
                if let (Some(e), Some(lens)) = (eye, &self.lens) {
                    let facing = ((e[6] - 0.15) / 0.3).clamp(0.0, 1.0);
                    if facing > 0.0 {
                        let m = Matrix3x2 {
                            M11: e[2] * w * k,
                            M12: e[3] * h * k,
                            M21: e[4] * w * k,
                            M22: e[5] * h * k,
                            M31: (x + e[0] * w) * k,
                            M32: (y + e[1] * h) * k,
                        };
                        dc.SetTransform(&(m * base));
                        dc.DrawBitmap(
                            lens.bitmap(),
                            Some(&D2D_RECT_F { left: -EYE_INSET, top: -EYE_INSET, right: EYE_INSET, bottom: EYE_INSET }),
                            facing,
                            D2D1_INTERPOLATION_MODE_LINEAR,
                            None,
                            None,
                        );
                    }
                }
                dc.SetTransform(&Matrix3x2::identity());
            }
            // искры — сложение света (globalCompositeOperation = 'lighter')
            if !self.sparks.is_empty() {
                let gen = self.glow_generation;
                let _ = self.glow_fx(cx, gen);
                let stroke = self.glow_fx.as_ref().map(|g| g.stroke.clone());
                dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_ADD);
                let acc = |a: f32| Color { r: ACC[0] as f32 / 255.0, g: ACC[1] as f32 / 255.0, b: ACC[2] as f32 / 255.0, a };
                for sp in &self.sparks {
                    let Some((px, py)) = sp.prev else { continue };
                    let (x, y) = sp.cur;
                    let p0 = D2D_POINT_2F { x: px * k, y: py * k };
                    let p1 = D2D_POINT_2F { x: x * k, y: y * k };
                    dc.DrawLine(p0, p1, cx.brush(acc(0.28 * sp.fade)), sp.size * 4.0 * k, stroke.as_ref());
                    dc.DrawLine(p0, p1, cx.brush(acc(sp.fade)), sp.size * k, stroke.as_ref());
                    if sp.size > 1.3 {
                        let r = sp.size * 0.7 * k;
                        let el = D2D1_ELLIPSE { point: p1, radiusX: r, radiusY: r };
                        dc.FillEllipse(&el, cx.brush(Color { r: 215.0 / 255.0, g: 1.0, b: 242.0 / 255.0, a: sp.fade }));
                    }
                }
                dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
            }
        }
    }
}
