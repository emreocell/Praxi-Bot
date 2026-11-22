import assert from "node:assert/strict";
import { test } from "node:test";
import type { AppSettings } from "../store/settingsStore.ts";
import { SETTINGS_SECTIONS } from "../views/settings/types.ts";
import {
  CARD_META,
  canReset,
  DETAIL_MODE_KEY,
  highlightParts,
  isCardShown,
  levelOfPreset,
  loadDetailMode,
  parseDetailMode,
  parsePage,
  RECOMMENDED_SAFETY_PRESET,
  resetPatch,
  SAFETY_LEVELS,
  saveDetailMode,
  SETTINGS_PAGE_GROUPS,
  SETTINGS_PAGES,
  pageOfSection
} from "./settingsPages.ts";

test("every page is in exactly one nav group", () => {
  const grouped = SETTINGS_PAGE_GROUPS.flatMap((g) => [...g.pages]);
  assert.equal(new Set(grouped).size, grouped.length);
  assert.deepEqual([...grouped].sort(), [...SETTINGS_PAGES].sort());
});

test("old section ids still open a page, and every section maps to a real page", () => {
  for (const s of SETTINGS_SECTIONS) assert.ok(SETTINGS_PAGES.includes(pageOfSection(s)));
  assert.equal(parsePage("ai"), "models");
  assert.equal(parsePage("computer"), "where");
  assert.equal(parsePage("safety"), "safety");
  assert.equal(parsePage("nonsense"), "general");
  assert.equal(parsePage(null), "general");
});

test("card placement only names real pages", () => {
  for (const [id, meta] of Object.entries(CARD_META)) {
    assert.ok(SETTINGS_PAGES.includes(meta.page), `${id} -> ${meta.page}`);
  }
});

test("advanced cards are hidden in Simple, shown in Advanced or while searching", () => {
  const adv = { page: "models", advanced: true } as const;
  assert.equal(isCardShown(adv, "simple", false), false);
  assert.equal(isCardShown(adv, "advanced", false), true);
  assert.equal(isCardShown(adv, "simple", true), true);
  assert.equal(isCardShown({ page: "models" }, "simple", false), true);
  // The Advanced page is already the expert page: nothing on it is hidden.
  assert.equal(isCardShown({ page: "advanced", advanced: true }, "simple", false), true);
});

test("Simple / Advanced is remembered, and a broken store falls back to Simple", () => {
  const mem = new Map<string, string>();
  const store = {
    getItem: (k: string) => mem.get(k) ?? null,
    setItem: (k: string, v: string) => void mem.set(k, v)
  };
  assert.equal(loadDetailMode(store), "simple");
  saveDetailMode("advanced", store);
  assert.equal(mem.get(DETAIL_MODE_KEY), "advanced");
  assert.equal(loadDetailMode(store), "advanced");
  assert.equal(parseDetailMode("weird"), "simple");
  const broken = {
    getItem: () => {
      throw new Error("blocked");
    },
    setItem: () => {
      throw new Error("blocked");
    }
  };
  assert.equal(loadDetailMode(broken), "simple");
  assert.doesNotThrow(() => saveDetailMode("advanced", broken));
});

test("the three safety levels map to daemon presets, balanced is the recommended default", () => {
  assert.deepEqual(
    SAFETY_LEVELS.map((l) => l.preset),
    ["read-and-draft", "approve-destructive", "full"]
  );
  assert.equal(SAFETY_LEVELS.filter((l) => l.recommended).length, 1);
  assert.equal(SAFETY_LEVELS.find((l) => l.recommended)?.preset, RECOMMENDED_SAFETY_PRESET);
  assert.equal(levelOfPreset("approve-destructive"), "balanced");
  // read-only and custom rules are not one of the three: the screen says "custom".
  assert.equal(levelOfPreset("read-only"), null);
  assert.equal(levelOfPreset(null), null);
});

const DEFAULTS = { speakReplies: false, voiceName: "", voiceRate: 1, wallpaperMotion: "cloud", language: "en", sendWith: "enter" } as unknown as AppSettings;

test("reset to recommended restores defaults only for the page's own settings", () => {
  const voice = resetPatch("voice", DEFAULTS);
  assert.deepEqual(Object.keys(voice ?? {}).sort(), ["speakReplies", "voiceName", "voiceRate"]);
  assert.equal(voice?.speakReplies, false);
  const appearance = resetPatch("appearance", DEFAULTS);
  assert.ok(appearance && "wallpaperMotion" in appearance);
  assert.ok(!("language" in (resetPatch("general", DEFAULTS) ?? {})), "language is not reset");
  assert.equal(resetPatch("models", DEFAULTS), null);
  assert.equal(canReset("safety"), true);
  assert.equal(canReset("advanced"), false);
});

test("search highlighting ignores accents and case and merges overlapping words", () => {
  assert.deepEqual(highlightParts("Gönder with Enter", "gonder"), [
    { text: "Gönder", hit: true },
    { text: " with Enter", hit: false }
  ]);
  assert.deepEqual(highlightParts("Send now", "se end"), [
    { text: "Send", hit: true },
    { text: " now", hit: false }
  ]);
  assert.deepEqual(highlightParts("Nothing", ""), [{ text: "Nothing", hit: false }]);
});
