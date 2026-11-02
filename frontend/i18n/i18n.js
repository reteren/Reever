// Перевод интерфейса без сборки: JSON-словари в i18n/locales/ читаются синхронно
// до первого кадра. Английский — эталон и запасной
// вариант: нет строки в выбранном языке — берётся английская, нет и её — ключ.
//
// Разметка переводится атрибутами:
//   data-i18n="key"             → textContent
//   data-i18n-html="key"        → innerHTML (только для своих строк с <br>, <strong>, <kbd>)
//   data-i18n-title / -placeholder / -aria-label="key" → соответствующий атрибут
(function () {
  // Языки интерфейса: все языки языковых пакетов Parakeet v3 и SenseVoice
  // (src-tauri/src/models.rs) и арабский. Порядок — как в uilang::SUPPORTED.
  const SUPPORTED = ['en', 'ru', 'de', 'es', 'pt', 'it', 'fr', 'zh', 'ja', 'ar',
    'bg', 'hr', 'cs', 'da', 'nl', 'et', 'fi', 'el', 'hu', 'lv', 'lt', 'mt', 'pl', 'ro',
    'sk', 'sl', 'sv', 'uk', 'ko', 'yue'];
  // Название языка — на нём самом: так его находит тот, кто не читает текущий.
  const NAMES = {
    en: 'English', ru: 'Русский', de: 'Deutsch', es: 'Español', pt: 'Português',
    it: 'Italiano', fr: 'Français', zh: '中文', ja: '日本語', ar: 'العربية',
    bg: 'Български', hr: 'Hrvatski', cs: 'Čeština', da: 'Dansk', nl: 'Nederlands',
    et: 'Eesti', fi: 'Suomi', el: 'Ελληνικά', hu: 'Magyar', lv: 'Latviešu',
    lt: 'Lietuvių', mt: 'Malti', pl: 'Polski', ro: 'Română', sk: 'Slovenčina',
    sl: 'Slovenščina', sv: 'Svenska', uk: 'Українська', ko: '한국어', yue: '粵語',
  };
  const RTL = new Set(['ar']);
  const dictionaries = {};
  const localeBase = new URL('locales/', document.currentScript.src);
  let locale = 'en';
  let dictionary = load('en');
  // Включённые языки интерфейса (settings.ui_languages); null — все.
  let enabled = null;

  function normalize(value) {
    const language = String(value || '').trim().toLowerCase().split(/[-_]/)[0];
    return SUPPORTED.includes(language) ? language : 'en';
  }

  function load(code) {
    if (dictionaries[code]) return dictionaries[code];
    const request = new XMLHttpRequest();
    try {
      request.open('GET', new URL(`${code}.json`, localeBase), false);
      request.send();
      if ((request.status === 200 || (request.status === 0 && request.responseText)) && request.responseText) {
        return (dictionaries[code] = JSON.parse(request.responseText));
      }
    } catch (error) {
      if (code === 'en') throw error;
    }
    if (code === 'en') throw new Error('Missing English interface dictionary');
    return null;
  }

  /** `auto` или пусто — язык Windows (его отдаёт WebView2), иначе код языка.
   *  Выключенный в Настройках язык — английский. */
  function resolve(preference) {
    const value = String(preference || '').trim();
    const code = normalize(!value || value === 'auto' ? navigator.language : value);
    return !enabled || enabled.includes(code) ? code : 'en';
  }

  /** Языки интерфейса, между которыми можно переключаться; английский — всегда. */
  function setEnabled(codes) {
    const chosen = Array.isArray(codes) ? codes.map(code => String(code).trim()) : null;
    enabled = chosen ? SUPPORTED.filter(code => code === 'en' || chosen.includes(code)) : null;
  }

  function setLanguage(preference) {
    const code = resolve(preference);
    const loaded = load(code);
    locale = loaded ? code : 'en';
    dictionary = loaded || dictionaries.en;
    document.documentElement.lang = locale;
    document.documentElement.dir = RTL.has(locale) ? 'rtl' : 'ltr';
    apply(document);
    return Promise.resolve(locale);
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
    SUPPORTED, NAMES, setLanguage, setEnabled, resolve, normalize, t, has, apply,
    get locale() { return locale; },
    get enabled() { return enabled || SUPPORTED; },
  };
  window.t = t;
})();
