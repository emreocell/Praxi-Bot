/* global console, process, URL */
// Verifies that every locale matches English and that all static t("...") keys exist.
//
// Strict locales must have the English keys, except the explicitly staged prefixes that ship
// in English and Turkish first. Scaffold locales
// (listed in src/i18n/scaffold-locales.json) are English copies that are translated part by part;
// a missing key there is only a warning, because the app falls back to English for it
// (i18n/index.ts: fallbackLng "en") and a translation that lags must never turn main red.
// Everything else stays an error for scaffolds too: extra keys, kind mismatches, mismatching
// {{placeholders}} and malformed JSON.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { usesEnglishFallback } from "../src/i18n/englishFallback.ts";

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
  es: new Set(["Chat"]),
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

// Turkish adds a case suffix to app names (GitHub'da), so the code also passes {{appAt}}; it stands for {{app}}.
const placeholders = (value) =>
  (value.replaceAll("{{appAt}}", "{{app}}").match(/\{\{[^}]*\}\}/g) ?? []).sort().join(" ");

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

const defaultLocaleDir = fileURLToPath(new URL("../src/i18n/locales/", import.meta.url));
const defaultScaffoldFile = fileURLToPath(new URL("../src/i18n/scaffold-locales.json", import.meta.url));

// The locale codes that are still scaffolds (see docs/contributing/adding-a-language.md).
// Without the file every locale is strict.
const readLocalePolicy = (file = defaultScaffoldFile) => {
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(file, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return { scaffold: [], englishFallbackPrefixes: [] };
    throw error;
  }
  if (!Array.isArray(parsed.scaffold) || parsed.scaffold.some((c) => typeof c !== "string")) {
    throw new Error(`${file}: expected {"scaffold": ["<locale code>", ...]}`);
  }
  const englishFallbackPrefixes = parsed.englishFallbackPrefixes ?? [];
  if (!Array.isArray(englishFallbackPrefixes) || englishFallbackPrefixes.some((p) => typeof p !== "string" || !p.endsWith("."))) {
    throw new Error(`${file}: expected {"englishFallbackPrefixes": ["<key prefix>.", ...]}`);
  }
  return { scaffold: parsed.scaffold, englishFallbackPrefixes };
};
export const readScaffoldLocales = (file) => readLocalePolicy(file).scaffold;

// Compares every locale file in `localeDir` with en.json. Returns the lines to print instead of
// printing them, so the rules can be tested: `errors` fail the check, `warnings` never do.
export const checkLocales = ({ localeDir = defaultLocaleDir, scaffold = readScaffoldLocales(), englishFallbackPrefixes = readLocalePolicy().englishFallbackPrefixes } = {}) => {
  const errors = [];
  const warnings = [];
  const read = (file) => JSON.parse(readFileSync(join(localeDir, file), "utf8"));
  const enJson = read("en.json");
  const enValues = new Map(flat(enJson));
  const enKinds = new Map(kinds(enJson));
  const localeFiles = readdirSync(localeDir)
    .filter((file) => file !== "en.json" && file.endsWith(".json") && statSync(join(localeDir, file)).isFile())
    .sort();
  const codes = localeFiles.map((file) => file.slice(0, -".json".length));
  for (const code of scaffold) {
    if (code === "en") errors.push("scaffold-locales.json: en is the reference and cannot be a scaffold");
    else if (!codes.includes(code)) errors.push(`scaffold-locales.json: ${code} has no file in the locales folder`);
  }
  for (const file of localeFiles) {
    const locale = file.slice(0, -".json".length);
    const isScaffold = scaffold.includes(locale);
    let json;
    try {
      json = read(file);
    } catch (error) {
      errors.push(`malformed JSON in ${file}: ${error.message}`);
      continue;
    }
    const values = new Map(flat(json));
    // Report-only: likely untranslated strings. Never counted as an error.
    const same = [...values].filter(
      ([key, value]) => typeof value === "string" && value === enValues.get(key) && !mayMatchEnglish(key, value, locale)
    );
    warnings.push(`${locale}: ${same.length} values identical to English`);
    for (const [key, value] of same) warnings.push(`warn ${locale}: ${key} = ${JSON.stringify(value)}`);
    const missing = [...enValues.keys()].filter((key) => !values.has(key));
    if (isScaffold) {
      if (missing.length) {
        warnings.push(
          `${locale} (scaffold): ${missing.length} keys missing, the app shows the English text for them. ` +
            `Copy them from en.json (or translate them), then run \`pnpm --dir apps/desktop/ui sort:i18n\`.`
        );
      }
      for (const key of missing) warnings.push(`warn missing in ${locale} (scaffold): ${key}`);
    } else {
      for (const key of missing) {
        if (usesEnglishFallback(locale, key)) continue;
        // The managed server flow ships in English and Turkish first.
        if (locale !== "tr" && englishFallbackPrefixes.some((prefix) => key.startsWith(prefix))) {
          warnings.push(`warn missing in ${locale} (English fallback): ${key}`);
        } else {
          errors.push(`missing in ${locale}: ${key}`);
        }
      }
    }
    const localeKinds = new Map(kinds(json));
    for (const [key, kind] of enKinds) {
      const other = localeKinds.get(key);
      if (other === undefined || other === kind) continue;
      errors.push(`kind mismatch in ${locale}: ${key} (expected ${kind}, got ${other})`);
    }
    for (const key of values.keys()) {
      if (!enValues.has(key)) errors.push(`extra in ${locale}: ${key}`);
    }
    for (const [key, value] of values) {
      const english = enValues.get(key);
      if (typeof value !== "string" || typeof english !== "string") continue;
      if (placeholders(english) !== placeholders(value)) {
        errors.push(`placeholders differ in ${locale}: ${key} (English has ${placeholders(english) || "none"}, ${locale} has ${placeholders(value) || "none"})`);
      }
    }
  }
  return { errors, warnings, keyCount: enValues.size, enKeys: new Set(enValues.keys()) };
};

// Every static t("...") call in the UI sources must name a key that exists in English.
export const findUnknownKeys = (root, enKeys) => {
  const walk = (d) =>
    readdirSync(d).flatMap((f) => {
      const p = join(d, f);
      return statSync(p).isDirectory() ? walk(p) : /\.(tsx?|ts)$/.test(f) ? [p] : [];
    });
  const unknown = new Set();
  const errors = [];
  for (const file of walk(root)) {
    const src = readFileSync(file, "utf8");
    for (const m of src.matchAll(/\bt\(\s*"([^"$]+)"/g)) {
      const key = m[1];
      if (enKeys.has(key) || unknown.has(key)) continue;
      unknown.add(key);
      errors.push(`${file}: unknown key ${key}`);
    }
  }
  return errors;
};

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const result = checkLocales();
  const errors = [
    ...result.errors,
    ...findUnknownKeys(fileURLToPath(new URL("../src/", import.meta.url)), result.enKeys),
  ];
  for (const line of result.warnings) console.warn(line);
  for (const line of errors) console.error(line);
  if (errors.length) process.exit(1);
  console.log(`i18n ok (${result.keyCount} keys)`);
}
