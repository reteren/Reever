// Правый клик не открывает меню WebView2 (Назад, Обновить, Печать, Средства разработчика) —
// в окне программы оно чужое и ломает страницу. В полях ввода меню остаётся: там оно даёт
// «Вырезать / Копировать / Вставить». Подключается в index.html, picker.html и confirm.html.
(() => {
  const EDITABLE = 'input, textarea, [contenteditable]:not([contenteditable="false"])';
  document.addEventListener('contextmenu', (e) => {
    if (!e.target.closest?.(EDITABLE)) e.preventDefault();
  });
})();
