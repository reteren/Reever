// Перетаскивание окна за любое пустое место: не только за отведённую полосу, а за всё,
// где под указателем нет кнопки, поля, кликабельной строки, полосы прокрутки или надписи.
// Подключается в index.html, picker.html и confirm.html — одинаково для полного и лёгкого вида.
(() => {
  const tauri = window.__TAURI__;
  const win = (() => { try { return tauri?.window?.getCurrentWindow?.() || null; } catch { return null; } })();
  if (!win) return;
  const CONTROLS = 'button, input, textarea, select, option, a, label, summary, [contenteditable], [tabindex], [role="button"], [role="tab"], .modal';
  const FREE_CURSORS = new Set(['auto', 'default']);

  // Курсор над глифом прямого текста элемента — это надпись, её не тащим (и её можно выделить).
  function overText(el, x, y) {
    for (const node of el.childNodes) {
      if (node.nodeType !== Node.TEXT_NODE || !node.textContent.trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      for (const r of range.getClientRects()) {
        if (x >= r.left - 2 && x <= r.right + 2 && y >= r.top - 2 && y <= r.bottom + 2) return true;
      }
    }
    return false;
  }

  // Клик по полосе прокрутки приходит на сам прокручиваемый элемент — за пределами clientWidth/Height.
  function overScrollbar(el, x, y) {
    const r = el.getBoundingClientRect();
    return (el.scrollHeight > el.clientHeight && x - r.left > el.clientLeft + el.clientWidth)
      || (el.scrollWidth > el.clientWidth && y - r.top > el.clientTop + el.clientHeight);
  }

  function draggable(e) {
    const el = e.target;
    if (!(el instanceof Element) || el === document.documentElement) return false;
    if (el.closest(CONTROLS)) return false;
    if (!FREE_CURSORS.has(getComputedStyle(el).cursor)) return false;
    if (overScrollbar(el, e.clientX, e.clientY) || overText(el, e.clientX, e.clientY)) return false;
    return true;
  }

  document.addEventListener('mousedown', e => {
    if (e.button !== 0 || e.defaultPrevented || !draggable(e)) return;
    // фокус из поля уходит, как при клике по пустому месту
    if (document.activeElement instanceof HTMLElement && document.activeElement !== document.body) document.activeElement.blur();
    // анимации встают сразу, до первого сдвига (skin/fps.js); дальше их держит reever://moving
    window.ReeverFps?.pause(300);
    win.startDragging().catch(() => {});
  });
})();
