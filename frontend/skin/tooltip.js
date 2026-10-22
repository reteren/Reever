/* Подсказки в стиле скина вместо системных: `title` любого элемента (в том числе
   выставленный i18n или добавленный позже) при наведении переезжает в `data-tip`, и
   вместо серой плашки Windows показывается маленький ЭЛТ-экран (.tip в skin.css). */
(() => {
  const DELAY = 380;     // первое наведение
  const WARM = 700;      // сколько после скрытия следующая подсказка — сразу
  const GAP = 8;
  const MARGIN = 6;

  const tip = document.createElement('div');
  tip.className = 'tip';
  tip.setAttribute('role', 'tooltip');
  tip.hidden = true;
  let target = null;
  let timer = 0;
  let hiddenAt = 0;

  function textOf(el) {
    const title = el.getAttribute('title');
    if (title) {
      el.dataset.tip = title;
      el.removeAttribute('title');
    }
    return el.dataset.tip || '';
  }

  function place() {
    const box = target.getBoundingClientRect();
    const own = tip.getBoundingClientRect();
    let top = box.bottom + GAP;
    let below = true;
    if (top + own.height > innerHeight - MARGIN && box.top - GAP - own.height >= MARGIN) {
      top = box.top - GAP - own.height;
      below = false;
    }
    const left = Math.min(Math.max(box.left + box.width / 2 - own.width / 2, MARGIN), innerWidth - own.width - MARGIN);
    tip.style.left = `${Math.round(left)}px`;
    tip.style.top = `${Math.round(top)}px`;
    tip.classList.toggle('above', !below);
  }

  function show() {
    if (!target?.isConnected) return;
    const text = textOf(target);
    if (!text) return;
    if (!tip.isConnected) document.body.append(tip);
    tip.textContent = text;
    tip.hidden = false;
    tip.classList.remove('on');
    place();
    requestAnimationFrame(() => tip.classList.add('on'));
  }

  function hide() {
    clearTimeout(timer);
    if (!tip.hidden) hiddenAt = performance.now();
    target = null;
    tip.hidden = true;
    tip.classList.remove('on');
  }

  document.addEventListener('mouseover', event => {
    const el = event.target.closest?.('[title], [data-tip]');
    if (el === target) return;
    hide();
    if (!el || !textOf(el)) return;
    target = el;
    timer = setTimeout(show, performance.now() - hiddenAt < WARM ? 0 : DELAY);
  });
  document.addEventListener('mouseout', event => {
    if (target && !target.contains(event.relatedTarget)) hide();
  });
  for (const type of ['mousedown', 'keydown', 'wheel', 'blur']) {
    addEventListener(type, hide, true);
  }
})();
