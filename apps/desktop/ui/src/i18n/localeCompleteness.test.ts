import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { usesEnglishFallback } from "./englishFallback.ts";

// Every strict language must provide English's keys and {{placeholders}}, except feature copy
// that ships with an explicit English fallback for that language (see englishFallback.ts and the
// staged prefixes listed alongside the scaffold locales).
// A missing key silently shows the English text (or the raw key). Scaffold locales (English copies
// being translated part by part, listed in scaffold-locales.json) may lag: the app falls back to
// English for them and scripts/check-i18n.mjs only warns. A locale that leaves that list is strict.
const localesDir = fileURLToPath(new URL("./locales/", import.meta.url));
const localePolicy: { scaffold: string[]; englishFallbackPrefixes: string[] } = JSON.parse(
  readFileSync(fileURLToPath(new URL("./scaffold-locales.json", import.meta.url)), "utf8"),
);
const TRANSLATED_LOCALES = readdirSync(localesDir)
  .filter((f) => f.endsWith(".json") && f !== "en.json")
  .map((f) => f.slice(0, -".json".length))
  .filter((code) => !localePolicy.scaffold.includes(code))
  .sort();
const LOCALES = ["en", ...TRANSLATED_LOCALES];

const load = (lng: string): unknown =>
  JSON.parse(readFileSync(fileURLToPath(new URL(`./locales/${lng}.json`, import.meta.url)), "utf8"));
const flat = (o: unknown, prefix = ""): Array<[string, string]> =>
  Object.entries(o as Record<string, unknown>).flatMap(([k, v]) =>
    v && typeof v === "object" ? flat(v, `${prefix}${k}.`) : [[`${prefix}${k}`, String(v)] as [string, string]],
  );
// Turkish adds a case suffix to app names (GitHub'da), so the code also passes {{appAt}}; it stands for {{app}}.
const placeholders = (value: string) =>
  (value.replaceAll("{{appAt}}", "{{app}}").match(/\{\{[^}]*\}\}/g) ?? []).sort().join(" ");

const maps: Record<string, Map<string, string>> = Object.fromEntries(
  LOCALES.map((l) => [l, new Map(flat(load(l)))]),
);

test("the strict locales include tr, de and es", () => {
  for (const code of ["tr", "de", "es"]) assert.ok(TRANSLATED_LOCALES.includes(code), `${code} must stay strict`);
});

test("every strict locale has all required English keys and no extras", () => {
  for (const lng of TRANSLATED_LOCALES) {
    const missing = [...maps.en.keys()].filter((k) => !maps[lng].has(k) && !usesEnglishFallback(lng, k)
      && (lng === "tr" || !localePolicy.englishFallbackPrefixes.some((prefix) => k.startsWith(prefix))));
    const extra = [...maps[lng].keys()].filter((k) => !maps.en.has(k));
    assert.deepEqual(missing, [], `${lng} is missing keys: ${missing.slice(0, 10).join(", ")}`);
    assert.deepEqual(extra, [], `${lng} has keys en does not: ${extra.slice(0, 10).join(", ")}`);
  }
});

test("the Composio hub uses English fallback only for German and Spanish", () => {
  assert.equal(usesEnglishFallback("de", "apps.composioHub.linkAccount"), true);
  assert.equal(usesEnglishFallback("es", "apps.composioHub.linkAccount"), true);
  assert.equal(usesEnglishFallback("tr", "apps.composioHub.linkAccount"), false);
  assert.equal(usesEnglishFallback("de", "apps.connect"), false);
});

test("no translation is empty and placeholders match English", () => {
  for (const lng of TRANSLATED_LOCALES) {
    const problems: string[] = [];
    for (const [key, en] of maps.en) {
      const value = maps[lng].get(key);
      if (value === undefined) continue; // reported by the test above
      if (en.trim() !== "" && value.trim() === "") problems.push(`${lng}: ${key} is empty`);
      if (placeholders(en) !== placeholders(value)) problems.push(`${lng}: ${key} placeholders differ`);
    }
    assert.deepEqual(problems, []);
  }
});
