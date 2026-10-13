// Перевод интерфейса без сборки: словари — обычные скрипты в i18n/locales/,
// каждый вызывает I18N.register(код, словарь). Английский — эталон и запасной
// вариант: нет строки в выбранном языке — берётся английская, нет и её — ключ.
//
// Разметка переводится атрибутами:
//   data-i18n="key"             → textContent
//   data-i18n-html="key"        → innerHTML (только для своих строк с <br>, <strong>, <kbd>)
//   data-i18n-title / -placeholder / -aria-label="key" → соответствующий атрибут
(function () {
  const SUPPORTED = ['en', 'ru', 'de', 'es', 'pt', 'it', 'fr', 'zh', 'ja', 'ar'];
  // Название языка — на нём самом: так его находит тот, кто не читает текущий.
  const NAMES = {
    en: 'English', ru: 'Русский', de: 'Deutsch', es: 'Español', pt: 'Português',
    it: 'Italiano', fr: 'Français', zh: '中文', ja: '日本語', ar: 'العربية',
  };
  const RTL = new Set(['ar']);
  const dictionaries = {};
  let locale = 'en';
  let dictionary = {};
  let request = 0;

  function normalize(value) {
    const language = String(value || '').trim().toLowerCase().split(/[-_]/)[0];
    return SUPPORTED.includes(language) ? language : 'en';
  }

  function register(code, entries) {
    dictionaries[code] = entries;
  }

  function load(code) {
    if (dictionaries[code]) return Promise.resolve();
    return new Promise(resolve => {
      const script = document.createElement('script');
      script.src = `i18n/locales/${code}.js`;
      // Нет файла — не беда: останется английский.
      script.onload = script.onerror = () => resolve();
      document.head.append(script);
    });
  }

  /** `auto` или пусто — язык Windows (его отдаёт WebView2), иначе код языка. */
  function resolve(preference) {
    const value = String(preference || '').trim();
    return normalize(!value || value === 'auto' ? navigator.language : value);
  }

  async function setLanguage(preference) {
    const code = resolve(preference);
    const current = ++request;
    await load('en');
    if (code !== 'en') await load(code);
    if (current !== request) return locale;
    locale = dictionaries[code] ? code : 'en';
    dictionary = dictionaries[locale] || {};
    document.documentElement.lang = locale;
    document.documentElement.dir = RTL.has(locale) ? 'rtl' : 'ltr';
    apply(document);
    return locale;
  }

  function pick(entry, params) {
    if (typeof entry === 'string') return entry;
    const count = typeof params.count === 'number' ? params.count : 0;
    const category = new Intl.PluralRules(locale).select(count);
    return entry[category] ?? entry.other ?? Object.values(entry)[0];
  }

  /** Перевод с подстановкой {name}; неизвестные {…} (например {q}) остаются как есть. */
  function t(key, params = {}) {
    let entry = dictionary[key];
    if (entry === undefined) entry = dictionaries.en?.[key];
    if (entry === undefined) return key;
    const text = String(pick(entry, params)).replace(/\{(\w+)\}/g, (placeholder, name) =>
      params[name] === undefined || params[name] === null ? placeholder : String(params[name]));
    // В арабском русская фраза в «» с цифрами перемешивается алгоритмом bidi
    // («33 умножить на 78» → «умножить на 78 33»): изолируем её направление.
    return RTL.has(locale) ? text.replace(/«([^»]*)»/g, '«⁨$1⁩»') : text;
  }

  function has(key) {
    return dictionary[key] !== undefined || dictionaries.en?.[key] !== undefined;
  }

  function apply(root) {
    root.querySelectorAll('[data-i18n]').forEach(node => { node.textContent = t(node.dataset.i18n); });
    root.querySelectorAll('[data-i18n-html]').forEach(node => { node.innerHTML = t(node.dataset.i18nHtml); });
    for (const [data, attribute] of [['i18nTitle', 'title'], ['i18nPlaceholder', 'placeholder'], ['i18nAriaLabel', 'aria-label']]) {
      const selector = `[data-${data.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase())}]`;
      root.querySelectorAll(selector).forEach(node => node.setAttribute(attribute, t(node.dataset[data])));
    }
  }

  window.I18N = {
    SUPPORTED, NAMES, register, setLanguage, resolve, normalize, t, has, apply,
    get locale() { return locale; },
  };
  window.t = t;
})();
