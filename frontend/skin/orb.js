// Линза распознавания. Ядро (WebGL2, под рамкой): стеклянный шар, в котором жидкий свет
// поднимается снизу вверх; рябь поверхности — от громкости голоса. Эффекты (Canvas 2D,
// над рамкой): частицы на орбитах в трёх плоскостях и светящиеся нити-следы вокруг линзы.
(function () {
  const ACC = [39, 230, 170];

  const VS = `#version 300 es
  in vec2 aPos; void main(){ gl_Position = vec4(aPos, 0.0, 1.0); }`;

  const FS = `#version 300 es
  precision highp float;
  uniform vec2 uRes; uniform float uTime;
  uniform float uLevel, uWave, uEnergy, uSwirl, uFlash, uDark, uGlitch;
  uniform vec3 uAcc;
  out vec4 outColor;

  float hash(float n){ return fract(sin(n) * 43758.5453); }
  float hash2(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
  float noise(vec2 p){
    vec2 i = floor(p), f = fract(p); f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash2(i), hash2(i + vec2(1, 0)), f.x), mix(hash2(i + vec2(0, 1)), hash2(i + vec2(1, 1)), f.x), f.y);
  }
  float fbm(vec2 p){ float s = 0.0, a = 0.5; for (int i = 0; i < 5; i++){ s += a * noise(p); p = p * 2.03 + 7.1; a *= 0.5; } return s; }

  void main(){
    vec2 q = (gl_FragCoord.xy / uRes) * 2.0 - 1.0;
    q /= 0.965;
    if (uGlitch > 0.0) {
      float band = floor(q.y * 16.0 + floor(uTime * 24.0));
      float on = step(0.72, hash(band * 1.37 + floor(uTime * 13.0)));
      q.x += (hash(band + floor(uTime * 21.0)) - 0.5) * 0.22 * uGlitch * on;
    }
    float r = length(q);
    if (r > 1.0) { outColor = vec4(0.0); return; }
    float z = sqrt(max(0.0, 1.0 - r * r));
    vec2 qi = q * (0.74 + 0.26 * z);                 // преломление: внутри шар «увеличивает»

    float ang = uSwirl * (uTime * 1.6 + (1.2 - r) * 2.4);
    mat2 rot = mat2(cos(ang), -sin(ang), sin(ang), cos(ang));
    vec2 qs = rot * qi;

    float surf = -1.0 + 2.0 * uLevel
      + uWave * (0.075 * sin(qi.x * 5.5 + uTime * 3.1) + 0.045 * sin(qi.x * 11.0 - uTime * 4.7) + 0.02 * sin(qi.x * 23.0 + uTime * 8.3))
      + 0.025 * sin(uTime * 0.9 + qi.x * 1.3);
    float inLiq = smoothstep(surf + 0.014, surf - 0.014, qi.y);
    float depth = clamp(surf - qi.y, 0.0, 2.0);

    float caus = fbm(qs * 2.7 + vec2(0.0, -uTime * 0.38));
    float caus2 = fbm(qs * 6.0 - vec2(uTime * 0.2, uTime * 0.55));
    vec3 liq = uAcc * (0.38 + 1.05 * caus + 0.35 * caus2) * (1.0 - 0.42 * depth);
    liq += uAcc * exp(-depth * 5.5) * 0.9 + vec3(0.55, 1.0, 0.85) * exp(-depth * 14.0) * 0.35;

    // пузырьки, всплывающие сквозь свет
    float bub = 0.0;
    for (int i = 0; i < 9; i++) {
      float fi = float(i);
      float bx = (hash(fi * 3.1) - 0.5) * 1.3 + 0.04 * sin(uTime * 2.0 + fi);
      float by = -1.0 + mod(uTime * (0.18 + 0.22 * hash(fi * 7.7)) + hash(fi * 5.3) * 2.0, 2.0);
      float d = length(qi - vec2(bx, by));
      float rr = 0.018 + 0.02 * hash(fi * 9.1);
      bub += smoothstep(rr, rr * 0.4, d) * step(by, surf);
    }
    liq += vec3(0.7, 1.0, 0.9) * bub * 0.5;

    float men = exp(-pow((qi.y - surf) / 0.02, 2.0));
    vec3 empty = vec3(0.003, 0.016, 0.013) + uAcc * 0.12 * exp(-max(qi.y - surf, 0.0) * 3.2) * uEnergy;
    vec3 col = mix(empty, liq * uEnergy, inLiq);
    col += (uAcc * 0.8 + vec3(0.45)) * men * (0.35 + uEnergy) * 0.75;
    col += uAcc * 0.22 * uEnergy * exp(-r * r * 2.2) * inLiq;

    // толстое стекло: тёмный край, френель, блики окна
    float fres = pow(1.0 - z, 2.4);
    col *= mix(1.0, 0.4, fres);
    col += vec3(0.7, 1.0, 0.92) * fres * 0.22 + uAcc * fres * 0.3 * uEnergy;
    vec2 hp = (q - vec2(-0.3, 0.44)) * vec2(1.0, 1.75);
    col += vec3(0.85, 1.0, 0.95) * smoothstep(0.44, 0.1, length(hp)) * 0.28 * (1.0 - uDark * 0.5);
    col += vec3(1.0) * smoothstep(0.075, 0.015, length(q - vec2(-0.42, 0.52))) * 0.95;
    col += uAcc * smoothstep(0.26, 0.0, length((q - vec2(0.46, -0.56)) * vec2(1.0, 2.1))) * 0.22 * uEnergy;

    col += (uAcc + vec3(0.65)) * uFlash * (1.0 - r * 0.45);
    col *= mix(1.0, 0.3, uDark);
    float alpha = smoothstep(1.0, 0.985, r);
    outColor = vec4(col * alpha, alpha);
  }`;

  function compile(gl, type, src) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    return s;
  }

  function createCore(canvas, size) {
    const gl = canvas.getContext('webgl2', { premultipliedAlpha: true, alpha: true });
    if (!gl) return { render() {}, resize() {} };
    const prog = gl.createProgram();
    gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
    gl.useProgram(prog);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, 'aPos');
    gl.enableVertexAttribArray(loc); gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    const U = {};
    for (const n of ['uRes', 'uTime', 'uLevel', 'uWave', 'uEnergy', 'uSwirl', 'uFlash', 'uDark', 'uGlitch', 'uAcc']) U[n] = gl.getUniformLocation(prog, n);
    gl.uniform3f(U.uAcc, ACC[0] / 255, ACC[1] / 255, ACC[2] / 255);
    function resize(scale) {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = canvas.height = Math.round(size * scale * dpr);
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.uniform2f(U.uRes, canvas.width, canvas.height);
    }
    function render(s) {
      gl.uniform1f(U.uTime, s.time);
      gl.uniform1f(U.uLevel, s.fill); gl.uniform1f(U.uWave, s.wave); gl.uniform1f(U.uEnergy, s.energy);
      gl.uniform1f(U.uSwirl, s.swirl); gl.uniform1f(U.uFlash, s.flash); gl.uniform1f(U.uDark, s.dark); gl.uniform1f(U.uGlitch, s.glitch);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    }
    return { render, resize };
  }

  // ── Частицы ─────────────────────────────────────────────────────────
  // Каждая частица живёт на своей наклонной орбите вокруг линзы; часть орбиты уходит
  // «за» шар — там частица не рисуется, и рой читается объёмным.
  function createFx(canvas, size, orbR) {
    const ctx = canvas.getContext('2d');
    const C = size / 2;
    const parts = [];
    const threads = [0, 1, 2, 3].map(i => ({ a: i * 1.7, w: (i % 2 ? -1 : 1) * (0.5 + i * 0.17), r: orbR + 26 + i * 9, len: 0.9 + i * 0.25, tilt: 0.25 + i * 0.22, on: 0 }));
    let scale = 1, dpr = 1;
    const rnd = (a, b) => a + Math.random() * (b - a);

    function spawn(mode) {
      if (mode === 'ember') {
        const a = rnd(Math.PI * 0.15, Math.PI * 0.85);
        return { kind: 'ember', x: C + Math.cos(a) * orbR * rnd(0.5, 1.05), y: C + Math.sin(a) * orbR * 0.9, vx: rnd(-6, 6), vy: rnd(-34, -14), life: 0, max: rnd(2.6, 5.5), size: rnd(0.8, 1.8), ph: rnd(0, 6.28) };
      }
      return {
        kind: 'orbit', a: rnd(0, Math.PI * 2), r: rnd(orbR + 12, orbR + 95), tilt: rnd(-1.15, 1.15), spin: rnd(0, Math.PI),
        w: rnd(0.5, 1.5) * (Math.random() < 0.82 ? 1 : -1), life: 0, max: rnd(2.5, 7), size: rnd(0.7, 2.6),
        vr: 0, px: null, py: null, ph: rnd(0, 6.28)
      };
    }

    function resize(s) {
      scale = s; dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = canvas.height = Math.round(size * s * dpr);
    }

    function render(st, dt) {
      ctx.setTransform(scale * dpr, 0, 0, scale * dpr, 0, 0);
      ctx.clearRect(0, 0, size, size);
      ctx.globalCompositeOperation = 'lighter';
      ctx.lineCap = 'round';

      // сколько частиц нужно в этом состоянии
      const want = st.mode === 'orbit' ? Math.round(150 + st.level * 160) : st.mode === 'collapse' ? 230 : st.mode === 'ember' ? 22 : 0;
      const kind = st.mode === 'ember' ? 'ember' : 'orbit';
      for (const p of parts) if (p.kind !== kind && !p.dying) p.dying = true;
      let alive = parts.filter(p => !p.dying).length;
      for (let k = 0; k < 8 && alive < want; k++, alive++) parts.push(spawn(kind));
      if (alive > want) { let extra = alive - want; for (const p of parts) { if (extra <= 0) break; if (!p.dying) { p.dying = true; extra--; } } }

      const speed = 1 + st.level * 2.6 + (st.mode === 'collapse' ? 1.5 : 0);
      for (let i = parts.length - 1; i >= 0; i--) {
        const p = parts[i];
        p.life += dt;
        let fade = Math.min(1, p.life / 0.35) * Math.min(1, (p.max - p.life) / 0.6);
        if (p.dying) { p.fadeOut = (p.fadeOut ?? 1) - dt * 2.2; fade *= Math.max(0, p.fadeOut); }
        if (p.life > p.max || (p.dying && p.fadeOut <= 0)) { parts.splice(i, 1); continue; }
        if (st.burst > 0 && p.kind === 'orbit') p.vr += st.burst * 2600 * dt;
        if (p.kind === 'ember') {
          p.x += (p.vx + Math.sin(p.life * 2 + p.ph) * 8) * dt; p.y += p.vy * dt;
          const a = 0.55 * fade;
          ctx.fillStyle = `rgba(${ACC[0]},${ACC[1]},${ACC[2]},${a})`;
          ctx.beginPath(); ctx.arc(p.x, p.y, p.size, 0, 6.283); ctx.fill();
          continue;
        }
        if (st.mode === 'collapse') { p.r -= (55 + 3000 / Math.max(p.r - orbR + 8, 6)) * dt; if (p.r < orbR * 0.86) { p.r = orbR + rnd(70, 110); p.px = null; } }
        p.r += p.vr * dt; p.vr *= Math.pow(0.04, dt);
        const jitter = Math.sin(p.life * 7 + p.ph) * st.level * 22;
        const w = p.w * speed * (st.mode === 'collapse' ? (orbR + 60) / Math.max(p.r, orbR * 0.8) : 1);
        p.a += w * dt;
        const rr = p.r + jitter;
        // орбита в наклонной плоскости: вращаем окружность вокруг оси под углом spin
        const ox = Math.cos(p.a) * rr, oy = Math.sin(p.a) * rr;
        const yz = oy * Math.cos(p.tilt), depth = oy * Math.sin(p.tilt);
        const x = C + ox * Math.cos(p.spin) - yz * Math.sin(p.spin);
        const y = C + ox * Math.sin(p.spin) + yz * Math.cos(p.spin);
        const behind = depth < 0 && Math.hypot(x - C, y - C) < orbR * 0.98;
        if (p.px != null && !behind) {
          const near = 0.55 + 0.45 * Math.max(-1, Math.min(1, depth / rr));
          const a = Math.min(1, fade * near * (0.5 + st.energy * 0.8));
          const lw = p.size * near * (1 + st.level * 0.9);
          // мягкое гало — широкий бледный штрих, потом яркое ядро
          ctx.strokeStyle = `rgba(${ACC[0]},${ACC[1]},${ACC[2]},${a * 0.22})`;
          ctx.lineWidth = lw * 4.5;
          ctx.beginPath(); ctx.moveTo(p.px, p.py); ctx.lineTo(x, y); ctx.stroke();
          ctx.strokeStyle = `rgba(${ACC[0]},${ACC[1]},${ACC[2]},${a})`;
          ctx.lineWidth = lw;
          ctx.stroke();
          if (p.size > 1.1) { ctx.fillStyle = `rgba(215,255,242,${a})`; ctx.beginPath(); ctx.arc(x, y, p.size * 0.75, 0, 6.283); ctx.fill(); }
        }
        // след длиннее на скорости: держим старую точку чуть дольше
        if (p.px == null || Math.hypot(x - p.px, y - p.py) > 2.5 + st.level * 8) { p.px = x; p.py = y; }
        if (behind) { p.px = null; }
      }

      // нити: длинные световые дуги по хромовому кольцу, как следы на «Elevator Star»
      for (const th of threads) {
        th.on += ((st.threads ? 1 : 0) - th.on) * Math.min(1, dt * 3);
        if (th.on < 0.01) continue;
        th.a += th.w * dt * (1 + st.level * 2);
        const steps = 34, span = th.len * (0.6 + 0.4 * th.on);
        let prev = null;
        for (let k = 0; k <= steps; k++) {
          const t = k / steps, a = th.a - span * t;
          const ox = Math.cos(a) * th.r, oy = Math.sin(a) * th.r * Math.cos(th.tilt);
          const x = C + ox, y = C + oy;
          if (prev) {
            const al = Math.min(1, (1 - t) * (1 - t) * th.on * (0.8 + st.level * 0.8));
            ctx.beginPath(); ctx.moveTo(prev[0], prev[1]); ctx.lineTo(x, y);
            ctx.strokeStyle = `rgba(${ACC.join(',')},${al * 0.25})`; ctx.lineWidth = 9 * (1 - t) + 1; ctx.stroke();
            ctx.strokeStyle = `rgba(${k < 3 ? '225,255,246' : ACC.join(',')},${al})`; ctx.lineWidth = 2.6 * (1 - t) + 0.4; ctx.stroke();
          }
          prev = [x, y];
        }
      }
      ctx.globalCompositeOperation = 'source-over';
    }
    return { render, resize, count: () => parts.length };
  }

  window.SkinOrb = { createCore, createFx };
})();
