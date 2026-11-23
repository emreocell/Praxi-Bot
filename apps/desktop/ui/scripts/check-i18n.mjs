/* global console, process, URL */
// Verifies that every locale matches English and that all static t("...") keys exist.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Values a locale may leave identical to English. A non-English value that equals the English
// one is reported as a warning ("warn ..."), because it often means the string was copied and
// never translated. The warning is only a hint and never changes the exit code; these rules
// silence the matches that are expected to be the same in every language.
//
// Brand, product and protocol names are written the same everywhere.
const SAME_AS_ENGLISH_VALUES = new Set([
  "Praxi", "Praxi-Bot", "Anthropic", "Google Gemini", "Ollama", "LM Studio",
  "OpenAI, OpenRouter, DeepSeek", "Composio", "AgentMail", "Telegram", "Node.js", "uv",
  "HTTP", "TLS", "STARTTLS", "IMAP", "SMTP", "SearXNG URL",
]);
// Words that really are the same in a given language (cognates and loan words everyone uses).
// Kept per language so a word that is a cognate in German is still flagged in Turkish.
const SAME_AS_ENGLISH_BY_LOCALE = {
  de: new Set([
    "Apps", "Computer", "Updates", "Port", "Name", "Version", "Neutral", "Warm", "Text", "Bots", "Bot",
    "Browser", "Transport", "Aurora", "Lava", "Limits", "Skills", "Avatar", "Desktop", "Remote", "Shell",
    "System", "Chats", "Chat", "Details", "Plan", "Terminal", "Global", "Tokens", "Limit", "Pause",
    "Websites", "Auto", "Online", "Offline", "online", "offline", "OK", "Problem", "Frost", "Neon",
    "Obsidian", "Rose", "Sand", "Gemini Live", "Cron", "Stdio", "min", "s", "LIVE", "Tokens", "Agent", "Relay", "Server", "fps", "ms", "B", "Enter",
  ]),
  tr: new Set([
    "Port", "Stdio", "Aurora", "Avatar", "Model", "Plan", "Terminal", "Bot", "Cron", "Limit", "Neon",
    "Gemini Live", "Enter", "B", "model",
  ]),
};
// Groups of keys whose values are names by design.
const SAME_AS_ENGLISH_KEY_PREFIXES = [
  "settings.language.", // each language's own name, the same in every locale on purpose
  "settingsModal.email.form.presets.", // mail providers (Gmail, iCloud, ...)
  "settingsModal.webSearch.backends.", // search engines and their APIs
];
// Keys ending in "Placeholder" hold example input (domains, app ids, model ids), not prose.
const isExamplePlaceholder = (key) => key.endsWith("Placeholder");
// Nothing to translate once {{placeholders}} are removed: "{{message}}", "×{{count}}", "".
const hasNoWords = (value) => !/\p{L}/u.test(value.replace(/\{\{[^}]*\}\}/g, ""));
// Keyboard shortcuts such as "Ctrl+Alt+Esc" or "Ctrl ,": key caps are not translated. Only
// combinations count, so a bare "Tab" or "Enter" (often a word some locales translate) still warns.
const KEY_CAP = String.raw`(?:Ctrl|Alt|Shift|Cmd|Esc|Enter|Tab|[A-Z0-9]|[,./])`;
const isShortcut = (value) => new RegExp(`^${KEY_CAP}(?:[+ ]${KEY_CAP})+$`).test(value);
// "Bots: {{names}}", "{{count}} B", "Terminal - {{name}}": every word is an allowed same-as-English word.
const onlyAllowedWords = (value, locale) => {
  const allowed = SAME_AS_ENGLISH_BY_LOCALE[locale];
  const words = value.replace(/\{\{[^}]*\}\}/g, " ").match(/\p{L}+/gu) ?? [];
  return !!allowed && words.length > 0 && words.every((w) => allowed.has(w));
};
const mayMatchEnglish = (key, value, locale) =>
  onlyAllowedWords(value, locale) ||
  SAME_AS_ENGLISH_VALUES.has(value) ||
  SAME_AS_ENGLISH_BY_LOCALE[locale]?.has(value) ||
  SAME_AS_ENGLISH_KEY_PREFIXES.some((prefix) => key.startsWith(prefix)) ||
  isExamplePlaceholder(key) ||
  hasNoWords(value) ||
  isShortcut(value);

const root = fileURLToPath(new URL("../src/", import.meta.url));
// Flattens nested objects into [dotted key, leaf value] pairs.
const flat = (o, p = "") =>
  Object.entries(o).flatMap(([k, v]) =>
    v && typeof v === "object" ? flat(v, `${p}${k}.`) : [[`${p}${k}`, v]]
  );
// Maps every node to its kind (array, object or string). Only plain objects are walked,
// so an array and an object with the same indices are still told apart.
const kindOf = (v) => (Array.isArray(v) ? "array" : v === null ? "null" : typeof v);
const kinds = (o, p = "") =>
  Object.entries(o).flatMap(([k, v]) => [
    [`${p}${k}`, kindOf(v)],
    ...(kindOf(v) === "object" ? kinds(v, `${p}${k}.`) : []),
  ]);
const localeDir = join(root, "i18n/locales");
const readValues = (file) => new Map(flat(JSON.parse(readFileSync(join(localeDir, file), "utf8"))));
const readKinds = (file) => new Map(kinds(JSON.parse(readFileSync(join(localeDir, file), "utf8"))));
const enValues = readValues("en.json");
const en = new Set(enValues.keys());
const enKinds = readKinds("en.json");
let bad = 0;
const localeFiles = readdirSync(localeDir).filter((file) => file !== "en.json" && file.endsWith(".json")).sort();
for (const file of localeFiles) {
  if (!statSync(join(localeDir, file)).isFile()) continue;
  const locale = file.slice(0, -".json".length);
  const values = readValues(file);
  const keys = new Set(values.keys());
  // Report-only: likely untranslated strings. Never counted in `bad`.
  const same = [...values].filter(
    ([key, value]) => typeof value === "string" && value === enValues.get(key) && !mayMatchEnglish(key, value, locale)
  );
  console.warn(`${locale}: ${same.length} values identical to English`);
  for (const [key, value] of same) console.warn(`warn ${locale}: ${key} = ${JSON.stringify(value)}`);
  for (const key of en) {
    if (keys.has(key)) continue;
    console.error(`missing in ${locale}: ${key}`);
    bad++;
  }
  const localeKinds = readKinds(file);
  for (const [key, kind] of enKinds) {
    const other = localeKinds.get(key);
    if (other === undefined || other === kind) continue;
    console.error(`kind mismatch in ${locale}: ${key} (expected ${kind}, got ${other})`);
    bad++;
  }
  for (const key of keys) {
    if (en.has(key)) continue;
    console.error(`extra in ${locale}: ${key}`);
    bad++;
  }
}

const walk = (d) =>
  readdirSync(d).flatMap((f) => {
    const p = join(d, f);
    return statSync(p).isDirectory() ? walk(p) : /\.(tsx?|ts)$/.test(f) ? [p] : [];
  });
const unknown = new Set();
for (const file of walk(root)) {
  const src = readFileSync(file, "utf8");
  for (const m of src.matchAll(/\bt\(\s*"([^"$]+)"/g)) {
    const key = m[1];
    if (en.has(key) || unknown.has(key)) continue;
    unknown.add(key);
    console.error(`${file}: unknown key ${key}`);
    bad++;
  }
}
if (bad) process.exit(1);
console.log(`i18n ok (${en.size} keys)`);
