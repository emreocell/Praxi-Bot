import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { ACCESS_LEVELS, accessLevelOf } from "./accessLevels.ts";

const locale = (lang: string) =>
  JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8")) as {
    accessLevel: Record<string, { name: string; line: string; can?: string[]; stops?: string[] }>;
  };

test("four levels, from the most freedom to the least; custom and unknown are not a level", () => {
  assert.deepEqual(ACCESS_LEVELS, ["full", "approve-destructive", "read-and-draft", "read-only"]);
  assert.equal(accessLevelOf("full"), "full");
  assert.equal(accessLevelOf("custom"), null);
  assert.equal(accessLevelOf(null), null);
  assert.equal(accessLevelOf("everything"), null);
});

test("every level has a name, one line, what it can do and what still stops it, in EN and TR", () => {
  for (const lang of ["en", "tr"]) {
    const levels = locale(lang).accessLevel;
    for (const id of ACCESS_LEVELS) {
      const l = levels[id];
      assert.ok(l?.name && l.line, `${lang} ${id}`);
      assert.ok((l.can?.length ?? 0) >= 2 && (l.stops?.length ?? 0) >= 2, `${lang} ${id} lists`);
    }
    assert.ok(levels.custom?.name && levels.custom.line, `${lang} custom`);
  }
});

// Owner decision (final): Full access asks only before spending money and finishes tasks alone.
test("Full access says it asks only before spending money and finishes tasks on its own", () => {
  const en = locale("en").accessLevel.full;
  assert.equal(en.name, "Full access");
  assert.equal(en.line, "Asks only before spending money; finishes tasks on its own.");
  const tr = locale("tr").accessLevel.full;
  assert.equal(tr.name, "Tam erişim");
  assert.match(tr.line, /yalnızca para harcamadan önce sorar/i);
  assert.match(tr.line, /kendi başına bitirir/i);
  // Money is the first thing that still stops it.
  assert.match(en.stops?.[0] ?? "", /money/i);
  assert.match(tr.stops?.[0] ?? "", /para/i);
});

// One list everywhere: every place that chooses a level uses the shared picker, every place that
// shows one uses the same names, and the old per-screen wordings are gone from the code.
test("the bot editor, the new-bot dialog and Settings choose with the same picker", () => {
  const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
  for (const file of ["../components/BotEditorFields.tsx", "../views/NewBotModal.tsx", "../views/settings/SafetyLevelsCard.tsx"]) {
    assert.match(read(file), /<AccessLevelPicker/, file);
  }
  for (const file of [
    "../components/BotPanel.tsx",
    "../views/BotProfileModal.tsx",
    "../views/SettingsSections.tsx",
    "../views/firstrun/FreedomStep.tsx",
    "../views/OnboardingView.tsx",
    "../views/NewBotModal.tsx",
    "../views/RecipeFlow.tsx"
  ]) {
    const src = read(file);
    assert.match(src, /accessLevelKey\(/, file);
    assert.doesNotMatch(src, /botForm\.preset\.|newBot\.autonomy|settingsModal\.safety\.levels/, file);
  }
  // The badge in the bot panel names the level and says what it means.
  assert.match(read("../components/BotPanel.tsx"), /accessLevel\.badge/);
});
