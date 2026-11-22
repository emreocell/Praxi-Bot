import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { FIRST_PROMPT_KEYS, keyShapeProblem } from "./firstRun.ts";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const locale = (lng: string): Record<string, Record<string, any>> =>
  JSON.parse(readFileSync(fileURLToPath(new URL(`../i18n/locales/${lng}.json`, import.meta.url)), "utf8"));

test("a key being typed gets plain advice before anything is sent", () => {
  assert.equal(keyShapeProblem("gemini", ""), null);
  assert.equal(keyShapeProblem("gemini", "https://aistudio.google.com/apikey"), "address");
  assert.equal(keyShapeProblem("gemini", "AIza short"), "address");
  assert.equal(keyShapeProblem("gemini", "AIzaSy123"), "short");
  assert.equal(keyShapeProblem("gemini", "sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaa"), "prefix");
  assert.equal(keyShapeProblem("gemini", "AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"), null);
  assert.equal(keyShapeProblem("anthropic", "sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaa"), null);
  assert.equal(keyShapeProblem("ollama", "x"), null);
  assert.equal(keyShapeProblem("openai", "sk-proj-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"), null);
});

test("the first-run flow ends with a Try this first card of four starter prompts in every language", () => {
  assert.equal(FIRST_PROMPT_KEYS.length, 4);
  const en = locale("en");
  for (const lng of ["en", "tr", "de"]) {
    const l = locale(lng);
    for (const key of FIRST_PROMPT_KEYS) {
      assert.ok(l.firstRun.prompts[key], `${lng} prompt ${key}`);
      if (lng !== "en") {
        assert.notEqual(l.firstRun.prompts[key], en.firstRun.prompts[key], `${lng} prompt ${key} translated`);
      }
    }
    assert.ok(l.firstRun.bot.tryTitle && l.firstRun.bot.whereBody && l.firstRun.bot.whereLater, `${lng} where/try copy`);
    assert.equal(l.botEmpty.suggestionsTitle, l.firstRun.bot.tryTitle);
    // The easy path: a free model with three steps, and a plain way to postpone.
    assert.equal(Object.keys(l.firstRun.brain.freeSteps).length, 3);
    assert.ok(l.firstRun.brain.laterNote.includes(l.firstRun.skip.replace(/[.]$/, "")), `${lng} later note names the skip button`);
  }
});

test("the wizard screens render the easy path, the where-it-runs note and the starter prompts", () => {
  const src = (f: string) => readFileSync(fileURLToPath(new URL(f, import.meta.url)), "utf8");
  assert.match(src("../views/firstrun/BrainStep.tsx"), /fr-gemini-steps/);
  assert.match(src("../views/firstrun/BrainStep.tsx"), /firstRun\.brain\.freeTitle/);
  assert.match(src("../views/firstrun/BotStep.tsx"), /fr-where/);
  assert.match(src("../views/firstrun/BotStep.tsx"), /FIRST_PROMPT_KEYS/);
});
