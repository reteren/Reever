// Хромовая рамка: рендер из Blender (design/reever-frame.blend) — цвет и карта нормалей,
// здесь переосвещается в реальном времени. Три источника: блик от курсора, бирюзовый
// свет линзы (рамка освещается голосом) и бегущая световая волна. При включении хром
// «наливается» от линзы наружу. Без WebGL2 — просто картинка, интерфейс работает.
(function () {
  const VS = `#version 300 es
  in vec2 aPos; void main(){ gl_Position = vec4(aPos, 0.0, 1.0); }`;

  const FS = `#version 300 es
  precision highp float;
  uniform sampler2D uColor, uNormal;
  uniform vec2 uCanvas;      // размер canvas в пикселях устройства
  uniform vec2 uSkin;        // размер скина в CSS-пикселях (1280x880)
  uniform vec2 uMouse; uniform float uMouseOn;
  uniform vec2 uOrb; uniform float uOrbI;
  uniform vec3 uAcc;
  uniform float uSweep, uSweepI;
  uniform float uReveal;     // радиус налива хрома от линзы, CSS px
  uniform vec3 uLunki[3];    // x, y, подсветка
  uniform float uTime;
  out vec4 outColor;

  void main(){
    vec2 uv = gl_FragCoord.xy / uCanvas; uv.y = 1.0 - uv.y;
    vec2 p = uv * uSkin;
    vec4 c = texture(uColor, uv);
    float a = c.a;
    if (a < 0.003) discard;
    vec3 n = normalize(texture(uNormal, uv).xyz * 2.0 - 1.0);
    vec3 V = vec3(0.0, 0.0, 1.0);
    vec3 add = vec3(0.0);

    // курсор — белый точечный свет над рамкой
    vec3 Lc = vec3(uMouse.x - p.x, -(uMouse.y - p.y), 230.0);
    float dc = length(Lc); Lc /= dc;
    float nh = max(dot(n, normalize(Lc + V)), 0.0);
    float fc = 1.0 / (1.0 + pow(dc / 380.0, 2.0));
    add += vec3(1.0, 1.0, 0.98) * (pow(nh, 110.0) * 1.25 + pow(nh, 14.0) * 0.10) * fc * uMouseOn;

    // линза — низкий бирюзовый свет, скользит по хрому рядом
    vec3 Lo = vec3(uOrb.x - p.x, -(uOrb.y - p.y), 36.0);
    float d2 = length(Lo); Lo /= d2;
    float lam = max(dot(n, Lo), 0.0);
    float so = pow(max(dot(n, normalize(Lo + V)), 0.0), 34.0);
    float fo = 1.0 / (1.0 + pow(d2 / 190.0, 2.0));
    add += uAcc * (lam * 0.32 + so * 1.7) * fo * uOrbI;

    // световая волна — вертикальная полоса света, бегущая по рамке
    float sx = uSweep * uSkin.x;
    vec3 Ls = normalize(vec3(sx - p.x, 40.0, 150.0));
    float ss = pow(max(dot(n, normalize(Ls + V)), 0.0), 60.0) * exp(-pow((p.x - sx) / 220.0, 2.0));
    add += mix(vec3(1.0), uAcc, 0.35) * ss * uSweepI * 1.4;

    // лунки — подсветка изнутри при наведении
    for (int i = 0; i < 3; i++) {
      vec3 lk = uLunki[i];
      if (lk.z > 0.001) {
        vec2 d = p - lk.xy;
        float f = exp(-dot(d, d) / (2.0 * 17.0 * 17.0));
        vec3 Ll = normalize(vec3(-d.x, d.y, 6.0));
        add += uAcc * lk.z * (max(dot(n, Ll), 0.0) * 1.2 + 0.25) * f;
      }
    }

    // на полупрозрачных кончиках шипов нормаль недостоверна — блики гасим, иначе искрят
    vec3 col = c.rgb + add * smoothstep(0.3, 0.9, a);

    // налив хрома от линзы: фронт светится, за фронтом — металл
    float dist = length(p - uOrb);
    float front = uReveal - dist;
    float shown = smoothstep(-6.0, 24.0, front);
    float edge = exp(-pow(front / 26.0, 2.0)) * step(-40.0, front);
    col = col * shown + (uAcc * 1.4 + vec3(0.45)) * edge * 0.9;
    float alpha = a * clamp(shown + edge, 0.0, 1.0);
    outColor = vec4(col * alpha, alpha);
  }`;

  function loadImage(url) {
    return new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve(img);
      img.onerror = () => reject(new Error('не загрузилось ' + url));
      img.src = url;
    });
  }

  function compile(gl, type, src) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    return s;
  }

  async function create(canvas, { color, normal, skin }) {
    const [imgC, imgN] = await Promise.all([loadImage(color), loadImage(normal)]);
    const gl = canvas.getContext('webgl2', { premultipliedAlpha: true, alpha: true, antialias: false });
    if (!gl) {
      // Без WebGL2 — рамка просто картинкой.
      canvas.replaceWith(Object.assign(imgC, { id: 'frame', style: 'position:absolute;left:0;top:0;width:1280px;height:880px;z-index:2;pointer-events:none' }));
      return { imgColor: imgC, render() {}, resize() {} };
    }
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

    const tex = (img, unit) => {
      const t = gl.createTexture();
      gl.activeTexture(gl.TEXTURE0 + unit); gl.bindTexture(gl.TEXTURE_2D, t);
      gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
      gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, img);
      gl.generateMipmap(gl.TEXTURE_2D);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    };
    tex(imgC, 0); tex(imgN, 1);
    const U = {};
    for (const name of ['uColor', 'uNormal', 'uCanvas', 'uSkin', 'uMouse', 'uMouseOn', 'uOrb', 'uOrbI', 'uAcc', 'uSweep', 'uSweepI', 'uReveal', 'uLunki', 'uTime'])
      U[name] = gl.getUniformLocation(prog, name);
    gl.uniform1i(U.uColor, 0); gl.uniform1i(U.uNormal, 1);
    gl.uniform2f(U.uSkin, skin.w, skin.h);
    gl.uniform3f(U.uAcc, 39 / 255, 230 / 255, 170 / 255);
    gl.enable(gl.BLEND); gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);

    function resize(scale) {
      // Не выше ×1,5: на экране 200 % шейдер рамки считает вдвое меньше пикселей, а хром с
      // бликами при растяжении почти не мягчеет. Вернуть ×2 — заменить 1.5 на 2.
      const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
      canvas.width = Math.round(skin.w * scale * dpr);
      canvas.height = Math.round(skin.h * scale * dpr);
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.uniform2f(U.uCanvas, canvas.width, canvas.height);
      lastKey = '';
    }

    // Шейдер запускается, только когда освещение изменилось: иначе на экране остаётся прошлый
    // кадр (холст без preserveDrawingBuffer не очищается, пока в него не рисуют).
    let lastKey = '';
    function render(s) {
      const q = v => Math.round(v * 1000);
      const key = [q(s.mouse.x), q(s.mouse.y), q(s.mouse.on), q(s.orb.x), q(s.orb.y), q(s.orbLight),
        q(s.sweep.x), q(s.sweep.i), q(s.reveal), ...s.lunki.map(q)].join(',');
      if (key === lastKey) return;
      lastKey = key;
      gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform2f(U.uMouse, s.mouse.x, s.mouse.y); gl.uniform1f(U.uMouseOn, s.mouse.on);
      gl.uniform2f(U.uOrb, s.orb.x, s.orb.y); gl.uniform1f(U.uOrbI, s.orbLight);
      gl.uniform1f(U.uSweep, s.sweep.x); gl.uniform1f(U.uSweepI, s.sweep.i);
      gl.uniform1f(U.uReveal, s.reveal);
      gl.uniform3fv(U.uLunki, s.lunki);
      gl.uniform1f(U.uTime, s.time);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    }
    return { imgColor: imgC, render, resize };
  }

  window.SkinFrame = { create };
})();
