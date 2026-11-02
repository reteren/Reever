// Полнота общих словарей окна, журнала и трея: у каждого языка ровно ключи английского, те же
// подстановки {name} и те же <теги>. Нет строки — пользователь увидит английскую,
// но заметит это только тот, кто откроет программу на этом языке; проверка
// показывает пробел всем сразу.
//
//   node scripts/check-i18n.mjs
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', 'frontend', 'i18n');
const SUPPORTED = ['en', 'ru', 'de', 'es', 'pt', 'it', 'fr', 'zh', 'ja', 'ar',
  'bg', 'hr', 'cs', 'da', 'nl', 'et', 'fi', 'el', 'hu', 'lv', 'lt', 'mt', 'pl', 'ro',
  'sk', 'sl', 'sv', 'uk', 'ko', 'yue'];

function load(code) {
  const entries = JSON.parse(readFileSync(join(root, 'locales', `${code}.json`), 'utf8'));
  if (!entries || Array.isArray(entries) || typeof entries !== 'object') {
    throw new Error(`${code}.json: expected an object`);
  }
  return entries;
}

const texts = entry => (typeof entry === 'string' ? [entry] : Object.values(entry));
const placeholders = entry => new Set(texts(entry).flatMap(text => text.match(/\{\w+\}/g) || []));
const tags = entry => new Set(texts(entry).flatMap(text => text.match(/<\/?\w+[^>]*>/g) || []));
const same = (a, b) => a.size === b.size && [...a].every(item => b.has(item));

const files = readdirSync(join(root, 'locales'));
const codes = files.filter(name => name.endsWith('.json')).map(name => name.replace(/\.json$/, ''));
const unknown = files.filter(name => !name.endsWith('.json') || !SUPPORTED.includes(name.replace(/\.json$/, '')));
const problems = unknown.map(name => `${name}: not in SUPPORTED`);
const reference = load('en');

for (const code of SUPPORTED.filter(code => code !== 'en')) {
  if (!codes.includes(code)) { problems.push(`${code}.json: missing`); continue; }
  let dictionary;
  try { dictionary = load(code); } catch (error) { problems.push(`${code}.json: ${error.message}`); continue; }
  const missing = Object.keys(reference).filter(key => !(key in dictionary));
  const extra = Object.keys(dictionary).filter(key => !(key in reference));
  if (missing.length) problems.push(`${code}: ${missing.length} missing keys: ${missing.slice(0, 8).join(', ')}`);
  if (extra.length) problems.push(`${code}: extra keys: ${extra.slice(0, 8).join(', ')}`);
  for (const [key, entry] of Object.entries(dictionary)) {
    if (!(key in reference)) continue;
    if (texts(entry).some(text => typeof text !== 'string' || !text.trim())) problems.push(`${code}: ${key} is empty`);
    if (typeof entry !== 'string' && !('other' in entry)) problems.push(`${code}: ${key} plural has no "other"`);
    // {count} в русском «{count} фр.» есть; подстановки не должны теряться или появляться.
    if (!same(placeholders(entry), placeholders(reference[key]))) problems.push(`${code}: ${key} placeholders differ from en`);
    if (!same(tags(entry), tags(reference[key]))) problems.push(`${code}: ${key} HTML tags differ from en`);
  }
}

if (problems.length) {
  console.error(problems.join('\n'));
  process.exit(1);
}
console.log(`i18n: ${SUPPORTED.length} languages × ${Object.keys(reference).length} keys — complete`);
