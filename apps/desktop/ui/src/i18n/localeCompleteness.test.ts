import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

// Every language must have exactly the keys English has, with the same {{placeholders}}.
// A missing key silently shows the English text (or the raw key) to a Turkish or German user.
const LOCALES = ["en", "tr", "de"] as const;

const load = (lng: string): unknown =>
  JSON.parse(readFileSync(fileURLToPath(new URL(`./locales/${lng}.json`, import.meta.url)), "utf8"));
const flat = (o: unknown, prefix = ""): Array<[string, string]> =>
  Object.entries(o as Record<string, unknown>).flatMap(([k, v]) =>
    v && typeof v === "object" ? flat(v, `${prefix}${k}.`) : [[`${prefix}${k}`, String(v)] as [string, string]],
  );
// Turkish adds a case suffix to app names (GitHub'da), so the code also passes {{appAt}}; it stands for {{app}}.
const placeholders = (value: string) =>
  (value.replaceAll("{{appAt}}", "{{app}}").match(/\{\{[^}]*\}\}/g) ?? []).sort().join(" ");

const maps = Object.fromEntries(LOCALES.map((l) => [l, new Map(flat(load(l)))])) as Record<
  (typeof LOCALES)[number],
  Map<string, string>
>;

test("tr and de have every key en has, and no extra keys", () => {
  for (const lng of ["tr", "de"] as const) {
    const missing = [...maps.en.keys()].filter((k) => !maps[lng].has(k));
    const extra = [...maps[lng].keys()].filter((k) => !maps.en.has(k));
    assert.deepEqual(missing, [], `${lng} is missing keys: ${missing.slice(0, 10).join(", ")}`);
    assert.deepEqual(extra, [], `${lng} has keys en does not: ${extra.slice(0, 10).join(", ")}`);
  }
});

test("no translation is empty and placeholders match English", () => {
  for (const lng of ["tr", "de"] as const) {
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
