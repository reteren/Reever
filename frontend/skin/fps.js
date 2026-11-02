// Частота кадров анимации интерфейса (настройка «Плавность»): 0 — как у монитора (каждый кадр
// requestAnimationFrame, на 180 Гц — 180 кадров), 60 или 30 — не чаще. Выбор живёт в localStorage:
// он общий для главного окна и окна маскота, смена в настройках долетает до маскота событием
// storage сразу, а распознавание из-за неё не перезапускается.
(function () {
  const KEY = 'reever.fpsLimit', ALLOWED = [0, 60, 30];
  const read = () => {
    try { const v = Number(localStorage.getItem(KEY)); return ALLOWED.includes(v) ? v : 0; } catch { return 0; }
  };
  let limit = read(), pausedUntil = 0;
  // Нативные окна (маскот без WebView2) localStorage не видят: выбор уходит в Rust при загрузке
  // окна и при каждой смене; Rust сохраняет его в settings.json (fps_limit), только если он другой.
  const tell = () => { try { window.__TAURI__?.core?.invoke('set_fps_limit', { limit }).catch(() => {}); } catch { } };
  tell();
  addEventListener('storage', (e) => { if (e.key === KEY) limit = read(); });
  window.ReeverFps = {
    get limit() { return limit; },
    set(value) {
      limit = ALLOWED.includes(Number(value)) ? Number(value) : 0;
      try { localStorage.setItem(KEY, String(limit)); } catch { }
      tell();
    },
    // Окно таскают: анимации стоят, пока оно движется, — перерисовка прозрачного окна на каждом
    // шаге перетаскивания и была рывками (shell.js зовёт pause на каждое событие сдвига).
    pause(ms) { pausedUntil = Math.max(pausedUntil, performance.now() + ms); },
    paused(now = performance.now()) { return now < pausedUntil; },
    // наименьший промежуток между кадрами, мс; запас 2 мс — чтобы дрожание кадров не роняло 60 до 30
    minMs() { return limit ? 1000 / limit - 2 : 0; },
  };
})();
