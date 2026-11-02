// Маскот — хромовый осьминог: общий рисовальщик для окна маскота (mascot.html) и сферы
// главного окна (shell.js). Пререндер из Blender (design: octobus chrome rig.blend):
//  grab_0001–0084        (Octo_Grab)  — подхват: 1–36 голова тянется, щупальца повисают; дальше — покачивание;
//  idle_0001–0096        (Octo_Idle)  — покой: переминается, поворачивается, перебирает щупальцами;
//  portal_in_0001–0058   (Octo_PortalIn + Portal_In, 48 к/с) — выход из портала;
//  portal_gate_0001–0058 (Portal_Out, 48 к/с) — портал ухода без осьминога: сам осьминог уходит
//    в него как есть — idle продолжает играть, а его сжимает и закручивает в центр портала здесь.
//  Кадры портала шире спрайта (328×408 при 100 %), центр у них общий.
// Жидкость в глазу — живая линза главного окна (skin/orb.js) с теми же состояниями распознавания;
// купол глаза по кадрам — mascot/anim.json (eye: центр и две полуоси эллипса, доли кадра, и насколько
// глаз смотрит в камеру). Свечение — размытый силуэт текущего кадра. Частицы портала — здесь же
// (центр и радиус портала по кадрам — anim.json, portal).
(function () {
  const FRAMES = 84, LOOP_FROM = 35, IDLE_FRAMES = 96, PORTAL_FRAMES = 58;
  const FPS = 24, PORTAL_FPS = 48;
  const RELEASE_FROM = 19;    // с какого кадра подхвата играть возврат задом наперёд
  const GRAB_SPEED = 1.5;     // подхват и возврат быстрее съёмки
  const EYE_INSET = 0.97;     // линза чуть меньше купола: край прячется под хромовое кольцо
  const GLOW = [70, 205, 165]; // акцент, приглушённый до матового
  const ACC = [39, 230, 170];
  const BASE = (() => { try { return new URL('.', document.currentScript.src).href.replace(/skin\/$/, ''); } catch { return ''; } })();

  // кадры общие для всех рисовальщиков на странице
  let shared = null;
  function assets() {
    if (shared) return shared;
    const load = (name, count) => Array.from({ length: count }, (_, i) => {
      const img = new Image();
      img.src = `${BASE}mascot/${name}_${String(i + 1).padStart(4, '0')}.webp`;
      return img;
    });
    shared = {
      sets: { grab: load('grab', FRAMES), idle: load('idle', IDLE_FRAMES), portal_in: load('portal_in', PORTAL_FRAMES), portal_gate: load('portal_gate', PORTAL_FRAMES) },
      anim: null,
    };
    fetch(`${BASE}mascot/anim.json`).then((r) => r.json()).then((a) => { shared.anim = a; }).catch((e) => console.error('mascot anim', e));
    return shared;
  }

  // ── Состояния — те же, что у линзы в главном окне (skin/shell.js) ───
  const STATES = {
    idle:      { fill: .17, energy: .55, wave: .14, swirl: 0, dark: 0, glitch: 0, glow: .45 },
    listening: { fill: .72, energy: 1.0, wave: .32, swirl: 0, dark: 0, glitch: 0, glow: .85 },
    busy:      { fill: .9, energy: 1.12, wave: .22, swirl: 1, dark: 0, glitch: 0, glow: 1 },
    loading:   { fill: .3, energy: .6, wave: .1, swirl: .3, dark: 0, glitch: 0, glow: .4 },
    paused:    { fill: 0, energy: .12, wave: 0, swirl: 0, dark: 1, glitch: 0, glow: .12 },
    error:     { fill: .22, energy: .5, wave: .1, swirl: 0, dark: .3, glitch: 1, glow: .35 },
  };
  const spring = (v, k, c) => ({ v, t: v, vel: 0, k, c });
  const stepSpring = (s, dt) => { const a = s.k * (s.t - s.v) - s.c * s.vel; s.vel += a * dt; s.v += s.vel * dt; return s.v; };
  const rnd = (a, b) => a + Math.random() * (b - a);

  // opts: glow — свечение по силуэту; onArrived / onGone — портал отыграл.
  function create(canvas, opts = {}) {
    const A = assets();
    const ctx = canvas.getContext('2d');
    // Размытый силуэт — широкое и узкое свечение; считаются при смене кадра спрайта, а не каждый
    // кадр, и в уменьшенном виде: широкое — в четверть, узкое — в половину размера холста, на
    // экран растягиваются обратно (мягкий ореол от этого не меняется, а blur() в 16 и 4 раза
    // дешевле). Спрайт рисуется прямо из картинки кадра; холсты без чтения с процессора
    // остаются на видеокарте.
    const GLOW_WIDE_DOWN = 4, GLOW_NEAR_DOWN = 2;
    const sil = document.createElement('canvas'), gctx = sil.getContext('2d');
    const glowWide = document.createElement('canvas'), wctx = glowWide.getContext('2d');
    const glowNear = document.createElement('canvas'), nctx = glowNear.getContext('2d');
    // проверка «курсор над головой» — по одному пикселю картинки кадра, по запросу
    const hit = document.createElement('canvas'); hit.width = hit.height = 1;
    const hctx = hit.getContext('2d', { willReadFrequently: true });
    const eyeCanvas = document.createElement('canvas');
    let eye = null;
    try { eye = window.SkinOrb.createCore(eyeCanvas, 40); } catch (e) { console.error('mascot orb', e); }

    // Геометрия в «базовых» CSS-пикселях (маскот при 100 %): холст W×H, спрайт покоя и кадр
    // портала с общим центром; k — масштаб базовых пикселей к CSS холста, clip — круг обрезки.
    let G = { W: 164, H: 204, spr: [22, 22, 120, 160], fly: [-82, -102, 328, 408], k: 1, clip: null };
    let dpr = 1, u = 1, drawn = null, drawnRect = null;

    const S = {
      fill: spring(0, 46, 7.5), energy: spring(0, 30, 10), wave: spring(0, 40, 12), swirl: spring(0, 10, 6),
      dark: spring(0, 18, 9), glitch: spring(0, 60, 14), glow: spring(0, 14, 7),
    };
    let state = 'idle', level = 0, levelRaw = 0, flash = 0;
    function setState(name) {
      state = STATES[name] ? name : 'idle';
      for (const key in S) S[key].t = STATES[state][key];
    }
    setState('idle');
    const ev = window.__TAURI__?.event;
    ev?.listen('reever://state', (e) => setState(String(e.payload)));
    ev?.listen('reever://level', (e) => {
      const rms = Number(e.payload) || 0;
      levelRaw = state === 'listening' || state === 'busy' ? Math.max(0, Math.min(1, (20 * Math.log10(rms + 1e-6) + 54) / 40)) : 0;
    });
    ev?.listen('reever://heard', (e) => { if (String(e.payload || '').trim()) flash = 1; });
    window.__TAURI__?.core?.invoke('get_ui_snapshot').then((s) => s && setState(String(s.state || 'idle'))).catch(() => {});

    // mode: rest | grab | release | portal_in | portal_out | gone
    let mode = 'gone', frame = 0, idleFrame = 0, acc = 0, last = 0;
    const t0 = performance.now();
    const inPortal = () => mode === 'portal_in' || mode === 'portal_out';

    function layout(g) {
      G = { ...G, ...g };
      dpr = window.devicePixelRatio || 1;
      u = G.k * dpr;
      const w = Math.round(G.W * G.k * dpr), h = Math.round(G.H * G.k * dpr);
      canvas.width = w; canvas.height = h;
      for (const [c, down] of [[sil, GLOW_NEAR_DOWN], [glowNear, GLOW_NEAR_DOWN], [glowWide, GLOW_WIDE_DOWN]]) {
        c.width = Math.max(1, Math.round(w / down)); c.height = Math.max(1, Math.round(h / down));
      }
      eye?.resize(G.k * (G.eyeScale || 1));
      drawn = null;
    }

    function current() {
      const a = A.anim;
      if (mode === 'gone') return null;
      if (mode === 'portal_in') return { img: A.sets.portal_in[frame], rect: G.fly, eye: a?.eye.portal_in?.[frame] };
      // уход: осьминог тот же, что и в покое, — портал позади, а сам он затягивается в него
      if (mode === 'rest' || mode === 'portal_out') return { img: A.sets.idle[idleFrame], rect: G.spr, eye: a?.eye.idle[idleFrame] };
      return { img: A.sets.grab[frame], rect: G.spr, eye: a?.eye.grab[frame] };
    }
    // силуэт и свечение перерисовываются, только когда сменился кадр
    function prepare(cur) {
      if (!cur.img.complete || !cur.img.naturalWidth) return false;
      if (drawn === cur.img && drawnRect === cur.rect) return true;
      if (opts.glow) {
        const [x, y, w, h] = cur.rect, s = u / GLOW_NEAR_DOWN;
        gctx.globalCompositeOperation = 'copy';
        gctx.drawImage(cur.img, x * s, y * s, w * s, h * s);
        gctx.globalCompositeOperation = 'source-in';
        gctx.fillStyle = `rgb(${GLOW.join(',')})`;
        gctx.fillRect(0, 0, sil.width, sil.height);
        gctx.globalCompositeOperation = 'source-over';
        for (const [g, c, px, down] of [[wctx, glowWide, 10, GLOW_WIDE_DOWN], [nctx, glowNear, 3, GLOW_NEAR_DOWN]]) {
          g.clearRect(0, 0, c.width, c.height);
          g.filter = `blur(${(px * u / down).toFixed(2)}px)`;
          g.drawImage(sil, 0, 0, c.width, c.height);
          g.filter = 'none';
        }
      }
      drawn = cur.img; drawnRect = cur.rect;
      return true;
    }
    function advance() {
      if (mode === 'rest') idleFrame = (idleFrame + 1) % IDLE_FRAMES;
      else if (mode === 'grab') frame = frame + 1 >= FRAMES ? LOOP_FROM : frame + 1;
      else if (mode === 'release') { frame = Math.max(0, frame - 1); if (frame === 0) { mode = 'rest'; idleFrame = 0; } }
      else if (inPortal()) {
        // покой продолжает играть и в портале ухода — в своём темпе (портал вдвое быстрее)
        if (mode === 'portal_out' && frame % 2) idleFrame = (idleFrame + 1) % IDLE_FRAMES;
        if (frame + 1 < PORTAL_FRAMES) frame++;
        else if (mode === 'portal_in') { mode = 'rest'; idleFrame = 0; frame = 0; opts.onArrived?.(); }
        else { mode = 'gone'; frame = 0; opts.onGone?.(); }
      }
    }

    // ── Частицы портала ─────────────────────────────────────────────
    // orbit — искры по кольцу; burst — выброс наружу, когда осьминог выходит и когда портал
    // схлопывается; suck — затягивание внутрь при уходе. Координаты — базовые пиксели.
    const sparks = [];
    const spawnAcc = { orbit: 0, burst: 0, suck: 0 };
    let popped = false;
    function portalNow() {
      const p = A.anim?.portal?.[mode]?.[frame];
      if (!p) return null;
      const [x, y, w, h] = G.fly;
      return { x: x + p[0] * w, y: y + p[1] * h, r: p[2] * (w / 328) };
    }
    function spawn(kind, P) {
      const a = rnd(0, Math.PI * 2), sc = G.fly[2] / 328;
      if (kind === 'orbit') sparks.push({ kind, a, rr: rnd(0.92, 1.12), w: rnd(2.2, 4.2), life: 0, max: rnd(0.35, 0.8), size: rnd(0.7, 1.8) * sc });
      else if (kind === 'suck') sparks.push({ kind, a, rr: rnd(1.3, 2.3), w: rnd(2.5, 4.5), vr: rnd(1.6, 2.6), life: 0, max: 1.4, size: rnd(0.7, 1.9) * sc });
      else {
        const sp = rnd(70, 230) * sc, r0 = P.r * rnd(0, 0.35);
        sparks.push({ kind, x: P.x + Math.cos(a) * r0, y: P.y + Math.sin(a) * r0, vx: Math.cos(a) * sp, vy: Math.sin(a) * sp, life: 0, max: rnd(0.4, 0.9), size: rnd(0.8, 2.2) * sc });
      }
    }
    function portalSparks(dt) {
      if (!inPortal()) { popped = false; return; }
      const P = portalNow();
      if (!P) return;
      const t = frame / (PORTAL_FRAMES - 1);
      const rate = { orbit: P.r > 6 ? 110 : 0, burst: 0, suck: 0 };
      if (mode === 'portal_in' && t > 0.14 && t < 0.55) rate.burst = 70;
      if (mode === 'portal_out' && t > 0.05 && t < 0.85) rate.suck = 140;
      for (const kind in rate) {
        spawnAcc[kind] += rate[kind] * dt;
        while (spawnAcc[kind] >= 1) { spawnAcc[kind] -= 1; spawn(kind, P); }
      }
      // схлопывание — последняя вспышка искр
      const closing = mode === 'portal_in' ? t > 0.88 : t > 0.93;
      if (closing && !popped) { popped = true; for (let i = 0; i < 26; i++) spawn('burst', { ...P, r: 4 }); }
      for (const sp of sparks) if (sp.kind !== 'burst') { sp.cx = P.x; sp.cy = P.y; sp.R = P.r; }
    }
    function drawSparks(dt) {
      if (!sparks.length) return;
      ctx.globalCompositeOperation = 'lighter';
      ctx.lineCap = 'round';
      for (let i = sparks.length - 1; i >= 0; i--) {
        const sp = sparks[i];
        sp.life += dt;
        let x, y;
        if (sp.kind === 'burst') {
          sp.vx *= Math.pow(0.12, dt); sp.vy *= Math.pow(0.12, dt);
          x = sp.x + sp.vx * dt; y = sp.y + sp.vy * dt;
        } else {
          if (sp.cx == null) { sparks.splice(i, 1); continue; }
          sp.a += sp.w * dt * (sp.kind === 'suck' ? 1 + 1.5 / Math.max(sp.rr, 0.2) : 1);
          if (sp.kind === 'suck') sp.rr -= sp.vr * dt;
          x = sp.cx + Math.cos(sp.a) * sp.rr * sp.R; y = sp.cy + Math.sin(sp.a) * sp.rr * sp.R;
        }
        const dead = sp.life > sp.max || (sp.kind === 'suck' && sp.rr < 0.08) || (sp.kind !== 'burst' && sp.R < 2);
        if (dead) { sparks.splice(i, 1); continue; }
        const fade = Math.min(1, sp.life / 0.12) * Math.min(1, (sp.max - sp.life) / 0.25);
        if (sp.px != null) {
          ctx.strokeStyle = `rgba(${ACC.join(',')},${0.28 * fade})`;
          ctx.lineWidth = sp.size * 4 * u;
          ctx.beginPath(); ctx.moveTo(sp.px * u, sp.py * u); ctx.lineTo(x * u, y * u); ctx.stroke();
          ctx.strokeStyle = `rgba(${ACC.join(',')},${fade})`;
          ctx.lineWidth = sp.size * u;
          ctx.stroke();
          if (sp.size > 1.3) { ctx.fillStyle = `rgba(215,255,242,${fade})`; ctx.beginPath(); ctx.arc(x * u, y * u, sp.size * 0.7 * u, 0, 6.283); ctx.fill(); }
        }
        sp.px = x; sp.py = y;
        if (sp.kind === 'burst') { sp.x = x; sp.y = y; }
      }
      ctx.globalCompositeOperation = 'source-over';
    }

    function draw(cur, time, dt) {
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.save();
      if (G.clip) { ctx.beginPath(); ctx.arc(G.clip.x * u, G.clip.y * u, G.clip.r * u, 0, 6.283); ctx.clip(); }
      // уход: позади — портал, осьминога сжимает и закручивает в его центр
      // (темп — как у Octo_PortalOut: стоит до 30 %, к 80 % исчезает, поворот 240°)
      let pull = null;
      if (mode === 'portal_out') {
        const gate = A.sets.portal_gate[frame], P = portalNow();
        if (gate?.complete && gate.naturalWidth) { const [x, y, w, h] = G.fly; ctx.drawImage(gate, x * u, y * u, w * u, h * u); }
        const t = frame / (PORTAL_FRAMES - 1), e = Math.max(0, Math.min(1, (t - 0.3) / 0.5)) ** 3;
        pull = { x: (P?.x ?? G.W / 2) * u, y: (P?.y ?? G.H / 2) * u, s: 1 - e, rot: -e * Math.PI * 4 / 3 };
        if (pull.s < 0.01) cur = null;
      }
      if (cur) {
        ctx.save();
        if (pull) { ctx.translate(pull.x, pull.y); ctx.rotate(pull.rot); ctx.scale(pull.s, pull.s); ctx.translate(-pull.x, -pull.y); }
        // свечение: мягко «дышит» и разгорается от голоса и принятой команды — яркостью готового ореола
        const g = Math.max(0, S.glow.v + flash * 0.8) * (0.85 + 0.15 * Math.sin(time * 1.7));
        if (opts.glow && g > 0.01) {
          ctx.globalAlpha = Math.min(1, 0.75 * g);
          ctx.drawImage(glowWide, 0, 0, canvas.width, canvas.height);
          ctx.globalAlpha = Math.min(1, 0.35 * g);
          ctx.drawImage(glowNear, 0, 0, canvas.width, canvas.height);
          ctx.globalAlpha = 1;
        }
        { const [x, y, w, h] = cur.rect; ctx.drawImage(cur.img, x * u, y * u, w * u, h * u); }
        // линза — по эллипсу купола; отвернувшийся глаз не светится
        const e = cur.eye;
        const facing = e ? Math.max(0, Math.min(1, (e[6] - 0.15) / 0.3)) : 0;
        if (eye && e && facing > 0) {
          eye.render({ time, fill: Math.max(0, S.fill.v), wave: S.wave.v, energy: Math.max(0, S.energy.v), swirl: S.swirl.v, flash, dark: Math.max(0, S.dark.v), glitch: Math.max(0, S.glitch.v) });
          const [x, y, w, h] = cur.rect;
          ctx.globalAlpha = facing;
          ctx.transform(e[2] * w * u, e[3] * h * u, e[4] * w * u, e[5] * h * u, (x + e[0] * w) * u, (y + e[1] * h) * u);
          ctx.drawImage(eyeCanvas, -EYE_INSET, -EYE_INSET, 2 * EYE_INSET, 2 * EYE_INSET);
          ctx.globalAlpha = 1;
        }
        ctx.restore();
      }
      drawSparks(dt);
      ctx.restore();
    }

    // своя петля; стоит, пока маскот невидим и искры догорели
    let running = false, loopFrame = 0;
    function tick(t) {
      loopFrame = 0;
      // каждый кадр монитора или не чаще ограничения из настройки «Плавность» (skin/fps.js)
      const minMs = window.ReeverFps?.minMs() || 0;
      if (last && minMs && t - last < minMs) { loopFrame = requestAnimationFrame(tick); return; }
      const dt = Math.min(0.05, last ? (t - last) / 1000 : 0.016);
      acc += dt; last = t;
      const fps = inPortal() ? PORTAL_FPS
        : mode === 'release' ? FPS * 1.6 * GRAB_SPEED
        : mode === 'grab' && frame < LOOP_FROM ? FPS * GRAB_SPEED
        : FPS;
      const step = 1 / fps;
      while (acc >= step) { acc -= step; advance(); }

      // уровень: быстрая атака, медленный спад — как в главном окне; доли — за кадр 60 Гц,
      // пересчёт на любую частоту
      level += (levelRaw - level) * (1 - Math.pow(levelRaw > level ? 0.45 : 0.88, dt * 60));
      const T = STATES[state], time = (t - t0) / 1000;
      const lv = state === 'listening' ? level : state === 'busy' ? level * 0.4 : 0;
      S.fill.t = T.fill + lv * 0.2 + (state === 'loading' ? Math.sin(time * 2.2) * 0.08 : 0);
      S.wave.t = T.wave + lv * 2.4;
      S.energy.t = T.energy + lv * 0.5;
      S.glow.t = T.glow + lv * 0.9;
      for (const key in S) stepSpring(S[key], dt);
      flash = Math.max(0, flash - dt * 2.6);

      portalSparks(dt);
      const cur = current();
      if (!cur) { draw(null, time, dt); drawn = drawnRect = null; }
      else if (prepare(cur)) draw(cur, time, dt);
      if (running && (mode !== 'gone' || sparks.length)) loopFrame = requestAnimationFrame(tick);
      else { ctx.clearRect(0, 0, canvas.width, canvas.height); running = false; }
    }
    function start() {
      if (running) return;
      running = true; last = 0;
      loopFrame = requestAnimationFrame(tick);
    }
    function stop() {
      running = false;
      if (loopFrame) cancelAnimationFrame(loopFrame);
      loopFrame = 0;
    }
    function clear() { ctx.clearRect(0, 0, canvas.width, canvas.height); drawn = drawnRect = null; }

    return {
      layout, start, stop, clear,
      get mode() { return mode; },
      get frame() { return frame; },
      inPortal,
      // прийти через портал / уйти в портал / сразу стоять / сразу исчезнуть
      arrive() { mode = 'portal_in'; frame = 0; acc = 0; drawn = drawnRect = null; clear(); start(); },
      depart() { if (mode === 'gone') return; mode = 'portal_out'; frame = 0; acc = 0; drawn = drawnRect = null; start(); },
      present() { if (mode === 'gone' || inPortal()) { mode = 'rest'; idleFrame = 0; frame = 0; } start(); },
      absent() { mode = 'gone'; frame = 0; sparks.length = 0; clear(); },
      grab() { if (mode !== 'grab') { mode = 'grab'; if (frame >= LOOP_FROM) frame = 0; } },
      drop() { mode = 'release'; frame = Math.min(frame, RELEASE_FROM); },
      // голова под точкой (CSS-координаты холста): выше линии HEAD_BOTTOM и непрозрачный пиксель
      onHead(x, y, headBottom) {
        if (inPortal() || mode === 'gone') return false;
        const [, sy, , sh] = G.spr;
        if (y < sy * G.k || y > (sy + sh * headBottom) * G.k) return false;
        if (!drawn || !drawnRect) return false;
        const [rx, ry, rw, rh] = drawnRect;
        const ix = (x / G.k - rx) / rw * drawn.naturalWidth, iy = (y / G.k - ry) / rh * drawn.naturalHeight;
        if (ix < 0 || iy < 0 || ix >= drawn.naturalWidth || iy >= drawn.naturalHeight) return false;
        hctx.clearRect(0, 0, 1, 1);
        hctx.drawImage(drawn, Math.floor(ix), Math.floor(iy), 1, 1, 0, 0, 1, 1);
        return hctx.getImageData(0, 0, 1, 1).data[3] > 40;
      },
    };
  }

  window.SkinMascot = { create };
})();
