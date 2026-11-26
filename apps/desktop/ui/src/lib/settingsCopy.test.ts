import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

/**
 * The owner's rule for Settings: a newcomer understands every option in five seconds. One short
 * sentence per option, no internal words; the details sit behind "How it works".
 */
const load = (lang: string) =>
  JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8")) as Record<string, unknown>;
const at = (o: Record<string, unknown>, key: string) =>
  key.split(".").reduce<unknown>((a, k) => (a && typeof a === "object" ? (a as Record<string, unknown>)[k] : undefined), o);
const flat = (o: unknown, p = "", out: Record<string, string> = {}) => {
  if (o && typeof o === "object") for (const [k, v] of Object.entries(o)) flat(v, p ? `${p}.${k}` : k, out);
  else if (typeof o === "string") out[p] = o;
  return out;
};

const OPTION_HINTS = [
  "memorySettings.modeHint.on",
  "memorySettings.modeHint.ask",
  "memorySettings.modeHint.off",
  "pageReader.modeHint.off",
  "pageReader.modeHint.auto",
  "pageReader.modeHint.always",
  "memory.description",
  "memory.pendingHint",
  "pageReader.description",
  "folderTrust.description",
  "settingsModal.computer.target.description",
  "workspace.cardDescription",
  "settingsModal.webSearch.description",
  "settingsModal.email.note"
];

for (const lang of ["en", "tr"]) {
  const strings = load(lang);
  test(`${lang}: each option and card hint is one short sentence`, () => {
    for (const key of OPTION_HINTS) {
      const text = at(strings, key);
      assert.equal(typeof text, "string", `${key} missing`);
      const value = text as string;
      assert.ok(value.length <= 110, `${key} is ${value.length} chars: ${value}`);
      const sentences = value.split(/[.!?](?:\s|$)/).filter((s) => s.trim().length > 0);
      assert.ok(sentences.length <= 2, `${key} has ${sentences.length} sentences: ${value}`);
    }
  });

  test(`${lang}: the rewritten Settings texts use no internal words`, () => {
    const jargon = /classifier|sınıflandırıcı|embedding|gömme|egress|taint|re-?index|dizinlen|recall preview|geri çağırma/i;
    const scopes = ["memory", "memorySettings", "memoryRecall", "pageReader", "choice", "folderTrust"];
    for (const scope of scopes) {
      for (const [key, value] of Object.entries(flat(at(strings, scope), scope))) {
        assert.doesNotMatch(value, jargon, `${key}: ${value}`);
      }
    }
  });

  test(`${lang}: "Recommended" is a badge, not part of an option name`, () => {
    assert.doesNotMatch(at(strings, "pageReader.mode.auto") as string, /\(/);
  });
}
