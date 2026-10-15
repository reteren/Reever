// Дирижёр скина: связывает события распознавания с рамкой, линзой, частицами,
// эквалайзером и экранами. Одна петля requestAnimationFrame; пока окно скрыто — стоит.
(function () {
  const tauri = window.__TAURI__;
  const invoke = tauri?.core?.invoke;
  const listen = tauri?.event?.listen || (async () => () => {});
  const win = (() => { try { return tauri?.window?.getCurrentWindow?.() || null; } catch { return null; } })();
  const $ = s => document.querySelector(s);
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  // Предпросмотр в браузере для снимков: ?shot — масштаб 1, без сцены включения.
  const params = new URLSearchParams(location.search);
  const SHOT = !tauri && params.has('shot');

  // Геометрия — та же, что в design/reever-frame.blend (CSS-пиксели скина).
  const SKIN = {
    w: 1280, h: 880,
    panel: { x0: 130, y0: 100, x1: 1150, y1: 780, r: 46 },
    orb: { x: 310, y: 352, r: 100 },
    core: { x: 204, y: 246, size: 212 },
    fx: { x: 10, y: 52, size: 600 },
    lunki: [[965, 100], [1015, 100], [1065, 100]],
  };
  // Компактная полоска — отдельный рендер той же сцены (коллекция Shade).
  const SHADE = {
    w: 640, h: 170,
    panel: { x0: 50, y0: 40, x1: 590, y1: 130, r: 45 },
    orb: { x: 112, y: 85, r: 30 },
    core: { size: 66 }, fx: { size: 220 },
    lunki: [[510, 85], [548, 85], [-999, -999]],
  };

  // ── Масштаб: скин целиком помещается в рабочую область экрана ───────
  let scale = 1;
  function fitScale() {
    const aw = (screen.availWidth || SKIN.w) - 24, ah = (screen.availHeight || SKIN.h) - 24;
    scale = SHOT ? 1 : Math.min(1, aw / SKIN.w, ah / SKIN.h);
    document.documentElement.style.setProperty('--s', scale);
    const Size = tauri?.dpi?.LogicalSize;
    if (win && Size && scale < 1) win.setSize(new Size(Math.round(SKIN.w * scale), Math.round(SKIN.h * scale))).catch(() => {});
  }

  // ── Маска окна по альфе рамки: пустоты между шипами пропускают клики ─
  function windowRects(img, G) {
    const W = G.w, H = G.h;
    const cv = document.createElement('canvas'); cv.width = W; cv.height = H;
    const g = cv.getContext('2d', { willReadFrequently: true });
    g.drawImage(img, 0, 0, W, H);
    const data = g.getImageData(0, 0, W, H).data;
    const on = new Uint8Array(W * H);
    for (let i = 0; i < W * H; i++) on[i] = data[i * 4 + 3] > 14 ? 1 : 0;
    // отверстия под экраны прозрачны в картинке, но это окно: вся плита — непрозрачная зона
    const P = G.panel;
    for (let y = P.y0; y < P.y1; y++) {
      const dy = Math.max(P.y0 + P.r - y, y - (P.y1 - P.r), 0);
      const inset = dy > 0 ? P.r - Math.sqrt(Math.max(0, P.r * P.r - dy * dy)) : 0;
      for (let x = Math.ceil(P.x0 + inset); x < P.x1 - inset; x++) on[y * W + x] = 1;
    }
    // +1 пиксель вокруг: сглаженный край хрома не должен обрезаться регионом
    const grown = new Uint8Array(W * H);
    for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
      if (!on[y * W + x]) continue;
      for (let dy = -1; dy <= 1; dy++) { const yy = y + dy; if (yy < 0 || yy >= H) continue; for (let dx = -1; dx <= 1; dx++) { const xx = x + dx; if (xx >= 0 && xx < W) grown[yy * W + xx] = 1; } }
    }
    // строки → отрезки → прямоугольники (одинаковые отрезки подряд склеиваются)
    const rects = []; let open = new Map();
    for (let y = 0; y <= H; y++) {
      const spans = [];
      if (y < H) { let x = 0; while (x < W) { if (grown[y * W + x]) { const s = x; while (x < W && grown[y * W + x]) x++; spans.push(s + ':' + x); } else x++; } }
      const next = new Map();
      for (const key of spans) { if (open.has(key)) { next.set(key, open.get(key)); open.delete(key); } else next.set(key, y); }
      for (const [key, y0] of open) { const [x0, x1] = key.split(':').map(Number); rects.push([x0 * scale, y0 * scale, x1 * scale, y * scale]); }
      open = next;
    }
    return rects;
  }
  async function applyShape(img, G) {
    if (!invoke || !img) return;
    try { await invoke('set_window_shape', { rects: windowRects(img, G) }); }
    catch (e) { console.error('window shape', e); }
  }

  // ── Пружины ─────────────────────────────────────────────────────────
  const spring = (v, k, c) => ({ v, t: v, vel: 0, k, c });
  function step(s, dt) { const a = s.k * (s.t - s.v) - s.c * s.vel; s.vel += a * dt; s.v += s.vel * dt; return s.v; }
  const S = {
    fill: spring(0, 46, 7.5),           // недодемпфировано: свет «взлетает» с перехлёстом
    energy: spring(0, 30, 10),
    wave: spring(0, 40, 12),
    swirl: spring(0, 10, 6),
    dark: spring(0, 18, 9),
    glitch: spring(0, 60, 14),
    orbLight: spring(0, 26, 9),
    mouseOn: spring(0, 20, 9),
    mx: spring(640, 90, 19), my: spring(200, 90, 19),
    l0: spring(0, 60, 13), l1: spring(0, 60, 13), l2: spring(0, 60, 13),
  };
  const STATES = {
    idle:      { fill: .17, energy: .55, wave: .14, swirl: 0, dark: 0, glitch: 0, orbLight: .35, mode: 'ember', threads: false },
    listening: { fill: .72, energy: 1.0, wave: .32, swirl: 0, dark: 0, glitch: 0, orbLight: .95, mode: 'orbit', threads: true },
    busy:      { fill: .9, energy: 1.12, wave: .22, swirl: 1, dark: 0, glitch: 0, orbLight: 1.15, mode: 'collapse', threads: true },
    loading:   { fill: .3, energy: .6, wave: .1, swirl: .3, dark: 0, glitch: 0, orbLight: .3, mode: 'ember', threads: false },
    paused:    { fill: 0, energy: .12, wave: 0, swirl: 0, dark: 1, glitch: 0, orbLight: 0, mode: 'none', threads: false },
    error:     { fill: .22, energy: .5, wave: .1, swirl: 0, dark: .3, glitch: 1, orbLight: .3, mode: 'ember', threads: false },
  };
  let state = 'idle', level = 0, levelRaw = 0, flash = 0, burst = 0;
  function setState(name) {
    state = STATES[name] ? name : 'idle';
    const T = STATES[state];
    for (const k of ['fill', 'energy', 'wave', 'swirl', 'dark', 'glitch', 'orbLight']) S[k].t = T[k];
    $('#ledRec')?.classList.toggle('on', state === 'listening' || state === 'busy');
    document.body.dataset.state = state;
  }

  // ── Эквалайзер: лента громкости голоса, справа — сейчас ─────────────
  function createEq(canvas) {
    const g = canvas.getContext('2d');
    const W = 924, H = 46, pitch = 9, cols = Math.floor((W - 16) / pitch), rows = 7;
    const hist = new Float32Array(cols);
    let acc = 0;
    function resize() { const dpr = Math.min(devicePixelRatio || 1, 2); canvas.width = W * scale * dpr; canvas.height = H * scale * dpr; g.setTransform(scale * dpr, 0, 0, scale * dpr, 0, 0); }
    function render(dt, lv, paused) {
      acc += dt;
      if (acc >= 0.045) { acc = 0; hist.copyWithin(0, 1); hist[cols - 1] = paused ? 0 : lv; }
      g.clearRect(0, 0, W, H);
      const top = 7, ph = (H - 14) / (rows - 1);
      for (let i = 0; i < cols; i++) {
        const v = hist[i], lit = Math.round(v * rows);
        const x = 8 + i * pitch + pitch / 2;
        const recent = i / cols;
        for (let r = 0; r < rows; r++) {
          const y = H - top - r * ph;
          if (r < lit) {
            const hot = r >= rows - 2;
            g.fillStyle = hot ? `rgba(200,255,236,${0.55 + 0.45 * recent})` : `rgba(39,230,170,${0.35 + 0.65 * recent})`;
            g.fillRect(x - 3, y - 1.6, 6, 3.2);
          } else {
            g.fillStyle = 'rgba(39,230,170,0.075)';
            g.fillRect(x - 3, y - 1.6, 6, 3.2);
          }
        }
      }
    }
    return { render, resize };
  }

  // ── Бегущая строка ─────────────────────────────────────────────────
  function fitTicker() {
    const el = $('#partial'), tape = $('#ticker .tape');
    if (!el || !tape) return;
    el.classList.remove('scroll');
    requestAnimationFrame(() => {
      const over = el.scrollWidth - tape.clientWidth;
      if (over > 4) {
        el.style.setProperty('--dist', -(over + 30) + 'px');
        el.style.setProperty('--dur', Math.max(4, (over + 30) / 55) + 's');
        el.classList.add('scroll');
      }
    });
  }

  // ── Подписи рельса: вертикальные, кегль подгоняется под длину слова на языке интерфейса
  function fitRail() {
    for (const span of document.querySelectorAll('.navi span')) {
      let fs = 17; span.style.setProperty('--nav-fs', fs + 'px');
      while (fs > 11 && span.scrollHeight > span.clientHeight + 1) { fs -= 1; span.style.setProperty('--nav-fs', fs + 'px'); }
    }
  }

  // ── Экран: смена канала и инверсия «команда принята» ───────────────
  function channel(swap) {
    const main = $('#main');
    if (reduced || !main) { swap(); return; }
    main.classList.remove('ch-in'); main.classList.add('ch-out');
    setTimeout(() => { swap(); main.classList.remove('ch-out'); void main.offsetWidth; main.classList.add('ch-in'); }, 140);
    sweepNow(0.55, 1.1);
  }
  function invertFlash() {
    const scr = $('#screen'); if (!scr || reduced) return;
    scr.classList.add('invert'); setTimeout(() => scr.classList.remove('invert'), 95);
  }

  // ── Свет ───────────────────────────────────────────────────────────
  const sweep = { x: -0.3, i: 0, speed: 0.8, active: false };
  function sweepNow(intensity = 0.7, speed = 0.75) { if (reduced || SHOT) return; sweep.x = -0.25; sweep.i = intensity; sweep.speed = speed; sweep.active = true; }

  // ── Петля ──────────────────────────────────────────────────────────
  let frame = null, core = null, fx = null, eq = null;
  const mainView = { frame: null, core: null, fx: null };
  const shadeView = { frame: null, core: null, fx: null };
  let shaded = false;
  let reveal = 0, revealTarget = 0, booted = false, running = false, last = 0, t0 = performance.now(), nextSweep = 6;
  const lunkiHover = [0, 0, 0];

  function loop(now) {
    if (!running) return;
    const dt = Math.min(0.05, (now - last) / 1000 || 0.016); last = now;
    const time = (now - t0) / 1000;
    // уровень: быстрая атака, медленный спад
    level += (levelRaw - level) * (levelRaw > level ? 0.55 : 0.12);
    const T = STATES[state];
    const lv = state === 'listening' ? level : state === 'busy' ? level * 0.4 : 0;
    S.fill.t = T.fill + lv * 0.2 + (state === 'loading' ? Math.sin(time * 2.2) * 0.08 : 0);
    S.wave.t = T.wave + lv * 2.4;
    S.orbLight.t = T.orbLight + lv * 1.3 + (orbHover ? 0.35 : 0);
    S.energy.t = T.energy + lv * 0.5;
    for (const k in S) step(S[k], dt);
    flash = Math.max(0, flash - dt * 2.6);
    burst = Math.max(0, burst - dt * 3);
    if (!reduced) { revealTarget = booted ? 1800 : revealTarget; reveal += (revealTarget - reveal) * Math.min(1, dt * 2.1); } else reveal = 1800;
    if (sweep.active) { sweep.x += dt * sweep.speed; if (sweep.x > 1.3) { sweep.active = false; sweep.i = 0; } }
    else if (time > nextSweep) { nextSweep = time + 9 + Math.random() * 6; sweepNow(0.45, 0.6); }
    for (let i = 0; i < 3; i++) S['l' + i].t = lunkiHover[i];

    // рисуется только видимый вид: полный скин или компактная полоска
    const V = shaded ? shadeView : mainView, G = shaded ? SHADE : SKIN;
    if (V.frame) V.frame.render({
      mouse: { x: S.mx.v, y: S.my.v, on: S.mouseOn.v }, orb: G.orb, orbLight: Math.max(0, S.orbLight.v + flash * 1.5),
      sweep, reveal, time,
      lunki: [G.lunki[0][0], G.lunki[0][1], S.l0.v, G.lunki[1][0], G.lunki[1][1], S.l1.v, G.lunki[2][0], G.lunki[2][1], S.l2.v],
    });
    if (V.core) V.core.render({ time, fill: Math.max(0, S.fill.v) * Math.min(1, reveal / 300), wave: S.wave.v, energy: Math.max(0, S.energy.v), swirl: S.swirl.v, flash, dark: Math.max(0, S.dark.v), glitch: Math.max(0, S.glitch.v) });
    if (V.fx) V.fx.render({ mode: booted ? T.mode : 'none', level: lv, energy: Math.max(0, S.energy.v), threads: T.threads && !shaded, burst }, dt);
    if (eq && !shaded) eq.render(dt, Math.min(1, level * 1.15), state === 'paused');
    requestAnimationFrame(loop);
  }
  function start() { if (running) return; running = true; last = performance.now(); requestAnimationFrame(loop); }
  function stop() { running = false; }

  // ── Включение ──────────────────────────────────────────────────────
  function powerOn(quick) {
    const order = ['#lcdTitle', '#lcdStatus', '#rail', '#screen', '#eq'];
    $('#skin').removeAttribute('data-booting');
    order.forEach((sel, i) => {
      const el = $(sel); if (!el) return;
      el.classList.remove('power'); void el.offsetWidth;
      if (reduced) return;
      el.style.animationDelay = (quick ? i * 0.05 : 0.45 + i * 0.11) + 's';
      el.classList.add('power');
    });
  }
  function boot() {
    booted = true;
    sweepNow(0.9, 0.62);
    powerOn(false);
    S.fill.v = 0; S.fill.vel = 0;
  }

  // ── Ввод ──────────────────────────────────────────────────────────
  let orbHover = false;
  document.addEventListener('mousemove', e => { S.mx.t = e.clientX / scale; S.my.t = e.clientY / scale; S.mouseOn.t = 1; });
  document.documentElement.addEventListener('mouseleave', () => { S.mouseOn.t = 0; });
  [...document.querySelectorAll('#skin .lunka'), ...document.querySelectorAll('#shade .lunka')].forEach((b, n) => {
    const i = n < 3 ? n : n - 3;
    b.addEventListener('mouseenter', () => { lunkiHover[i] = 1.2; });
    b.addEventListener('mouseleave', () => { lunkiHover[i] = 0; });
    b.addEventListener('mousedown', () => { lunkiHover[i] = 2.2; });
    b.addEventListener('mouseup', () => { lunkiHover[i] = 1.2; });
  });
  const orbHit = $('#orbHit');
  orbHit.addEventListener('mouseenter', () => { orbHover = true; });
  orbHit.addEventListener('mouseleave', () => { orbHover = false; });
  orbHit.addEventListener('click', () => { $('#pauseBtn')?.click(); flash = 0.6; });
  const shadeOrb = $('#shadeOrb');
  shadeOrb.addEventListener('mouseenter', () => { orbHover = true; });
  shadeOrb.addEventListener('mouseleave', () => { orbHover = false; });
  shadeOrb.addEventListener('click', () => { $('#pauseBtn')?.click(); flash = 0.6; });
  $('#shadeDrag').addEventListener('mousedown', e => { if (e.button === 0 && win) win.startDragging().catch(() => {}); });

  // ── Компактный режим: окно сжимается в хромовую полоску поверх всех окон ──
  async function setShade(on) {
    if (on === shaded) return;
    const Size = tauri?.dpi?.LogicalSize;
    if (on && !shadeView.frame) {
      try {
        shadeView.core = window.SkinOrb.createCore($('#shadeCore'), SHADE.core.size); shadeView.core.resize(scale);
        shadeView.fx = window.SkinOrb.createFx($('#shadeFx'), SHADE.fx.size, SHADE.orb.r); shadeView.fx.resize(scale);
        shadeView.frame = await window.SkinFrame.create($('#shadeFrame'), { color: 'skin/shade_color.webp', normal: 'skin/shade_normal.webp', skin: SHADE });
        shadeView.frame.resize(scale);
      } catch (e) { console.error('shade', e); return; }
    }
    shaded = on;
    const G = on ? SHADE : SKIN;
    // сначала регион, потом размер: иначе на кадр мелькнёт старая форма в новом размере
    if (on) await applyShape(shadeView.frame?.imgColor, SHADE);
    if (win && Size) await win.setSize(new Size(Math.round(G.w * scale), Math.round(G.h * scale))).catch(() => {});
    if (!on) await applyShape(mainView.frame?.imgColor, SKIN);
    win?.setAlwaysOnTop(on).catch(() => {});
    document.body.classList.toggle('shaded', on);
    syncShadeText();
    if (!reduced && !SHOT) { const el = on ? $('#shade') : null; if (el) { el.classList.remove('power'); void el.offsetWidth; el.classList.add('power'); } else powerOn(true); }
    sweepNow(0.8, 1.2);
  }
  function syncShadeText() {
    const partial = $('#partial')?.textContent.trim();
    const heard = $('#lastCmd')?.dataset.heard ? $('#lastCmd').textContent.trim() : '';
    const text = partial && partial !== '…' ? partial : heard || $('#micLabel')?.textContent || 'REEVER';
    const el = $('#shadeText'); if (!el || el.textContent === text) return;
    el.textContent = text;
    el.classList.remove('scroll');
    requestAnimationFrame(() => {
      const over = el.scrollWidth - el.parentElement.clientWidth;
      if (over > 4) { el.style.setProperty('--dist', -(over + 24) + 'px'); el.style.setProperty('--dur', Math.max(4, (over + 24) / 50) + 's'); el.classList.add('scroll'); }
    });
  }
  $('#winShade').addEventListener('click', () => setShade(true));
  $('#shadeOpen').addEventListener('click', () => setShade(false));
  $('#shadeTray').addEventListener('click', () => { try { win?.hide(); } catch (e) { console.error(e); } });
  for (const sel of ['#partial', '#lastCmd', '#micLabel']) new MutationObserver(() => shaded && syncShadeText()).observe($(sel), { childList: true, characterData: true, subtree: true });
  $('#drag').addEventListener('mousedown', e => { if (e.button === 0 && win) win.startDragging().catch(() => {}); });

  // ── События распознавания ─────────────────────────────────────────
  listen('reever://state', e => setState(String(e.payload)));
  listen('reever://level', e => { const rms = Number(e.payload) || 0; levelRaw = Math.max(0, Math.min(1, (20 * Math.log10(rms + 1e-6) + 54) / 40)); });
  listen('reever://heard', e => { if (String(e.payload || '').trim()) { flash = 1; burst = 1; invertFlash(); sweepNow(0.8, 1.2); } });
  listen('reever://partial', () => fitTicker());
  listen('reever://status', e => {
    let s = e.payload; if (typeof s === 'string') { try { s = JSON.parse(s); } catch { return; } }
    $('#ledNet')?.classList.toggle('on', s?.everything === true);
  });
  new MutationObserver(fitRail).observe($('#rail'), { childList: true, characterData: true, subtree: true });
  new MutationObserver(fitTicker).observe($('#partial'), { childList: true, characterData: true, subtree: true });

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) { stop(); return; }
    start();
    if (booted) { powerOn(true); sweepNow(0.7, 0.9); }
  });

  // ── Старт ─────────────────────────────────────────────────────────
  window.Skin = { channel, setState, sweep: sweepNow, shade: setShade };
  (async () => {
    fitScale();
    setState('idle');
    try { $('#ver').textContent = invoke ? 'v' + await tauri.app.getVersion() : 'preview'; } catch { }
    try {
      core = mainView.core = window.SkinOrb.createCore($('#orbCore'), SKIN.core.size); core.resize(scale);
      fx = mainView.fx = window.SkinOrb.createFx($('#orbFx'), SKIN.fx.size, SKIN.orb.r); fx.resize(scale);
    } catch (e) { console.error('orb', e); }
    eq = createEq($('#eqCanvas')); eq.resize();
    start();
    try {
      frame = mainView.frame = await window.SkinFrame.create($('#frame'), { color: 'skin/frame_color.webp', normal: 'skin/frame_normal.webp', skin: SKIN });
      frame.resize(scale);
      applyShape(frame.imgColor, SKIN);
    } catch (e) { console.error('frame', e); }
    await document.fonts.ready;
    fitRail();
    // демо-состояния для предпросмотра без Tauri: ?demo=listening|busy|paused|…
    const demo = params.get('demo');
    if (!invoke && (demo || SHOT)) {
      if (SHOT) { document.body.style.background = params.get('bg') || '#2a2f33'; booted = true; revealTarget = reveal = 1800; $('#skin').removeAttribute('data-booting'); }
      else boot();
      setState(demo || 'idle');
      if (demo === 'listening') setInterval(() => { levelRaw = 0.35 + 0.6 * Math.abs(Math.sin(performance.now() / 260)) * Math.random(); }, 60);
      if (params.get('heard')) { const h = $('#lastCmd'); h.dataset.heard = '1'; h.textContent = params.get('heard'); $('#partial').textContent = params.get('heard'); }
      // снимок без живого времени: прогреть рой частиц и ленту эквалайзера
      if (SHOT && fx) { const T = STATES[demo || 'idle']; for (let i = 0; i < 240; i++) { if (demo === 'listening') levelRaw = 0.3 + 0.6 * Math.abs(Math.sin(i / 7)) * (0.6 + 0.4 * Math.random()); level += (levelRaw - level) * 0.4; fx.render({ mode: T.mode, level: demo === 'listening' ? level : 0, energy: T.energy, threads: T.threads, burst: 0 }, 1 / 60); eq?.render(1 / 20, demo === 'listening' ? level : 0.05 * Math.random(), demo === 'paused'); } }
      if (params.has('debug')) { const errs = []; addEventListener('error', e => errs.push(e.message)); setTimeout(() => { document.title = 'fx:' + (fx?.count?.() ?? 'none') + ' err:' + errs.join('|'); }, 2000); }
      if (params.has('shade')) await setShade(true);
      if (params.get('nav')) { const ch = window.Skin.channel; window.Skin.channel = f => f(); document.querySelector(`[data-nav="${params.get('nav')}"]`)?.click(); window.Skin.channel = ch; }
      return;
    }
    if (document.hidden) {
      const once = () => { if (!document.hidden) { document.removeEventListener('visibilitychange', once); boot(); } };
      document.addEventListener('visibilitychange', once);
    } else boot();
  })();
})();
