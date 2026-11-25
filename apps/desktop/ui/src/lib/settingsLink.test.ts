import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { matchesQuery } from "./searchText.ts";
import {
  openSettingsTarget,
  OPEN_SETTINGS_EVENT,
  peekPendingTarget,
  takePendingFocus,
  VOICE_KEY_TARGET
} from "./settingsLink.ts";
import { CARD_META, SETTINGS_PAGES } from "./settingsPages.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

test("the voice key and API key deep links point at cards and fields that exist", () => {
  assert.ok(SETTINGS_PAGES.includes(VOICE_KEY_TARGET.page));
  assert.equal(CARD_META[VOICE_KEY_TARGET.card]?.page, VOICE_KEY_TARGET.page);
  assert.equal(CARD_META[VOICE_KEY_TARGET.card]?.advanced, undefined, "must not be hidden behind More options");
  assert.match(read("../views/settings/GeneralCards.tsx"), new RegExp(`"${VOICE_KEY_TARGET.card}",`));
  assert.match(read("../views/settings/DictationCard.tsx"), new RegExp(`data-settings-field="${VOICE_KEY_TARGET.field}"`));
  assert.ok(read("../views/settings/DictationCard.tsx").includes(`takePendingFocus("${VOICE_KEY_TARGET.field}")`));

  assert.equal(CARD_META.keys?.page, "models");
  assert.match(read("../views/settings/ApiKeyPaste.tsx"), /data-settings-field="api-key"/);
  assert.ok(read("../views/settings/ApiKeyPaste.tsx").includes('takePendingFocus("api-key")'));
});

test("Settings search finds the dictation card by dikte, ses, gemini and voice", () => {
  const keywords = /"dictation",[\s\S]*?keywords: "([^"]+)"/.exec(read("../views/settings/GeneralCards.tsx"))?.[1];
  assert.ok(keywords);
  for (const q of ["dikte", "ses", "gemini", "voice", "api key", "anruf"]) {
    assert.ok(matchesQuery(q, keywords), q);
  }
});

test("openSettingsTarget announces the target once, peeks safely, and the field focus is taken once", () => {
  const seen: unknown[] = [];
  const g = globalThis as unknown as { window: unknown; CustomEvent: unknown };
  g.CustomEvent = class {
    type: string;
    detail: unknown;
    constructor(t: string, i: { detail: unknown }) {
      this.type = t;
      this.detail = i.detail;
    }
  };
  g.window = {
    dispatchEvent: (e: { type: string; detail: unknown }) => {
      if (e.type === OPEN_SETTINGS_EVENT) seen.push(e.detail);
      return true;
    }
  };
  openSettingsTarget(VOICE_KEY_TARGET);
  assert.deepEqual(seen, [VOICE_KEY_TARGET]);
  assert.deepEqual(peekPendingTarget(), VOICE_KEY_TARGET);
  assert.equal(takePendingFocus("other"), false);
  assert.equal(takePendingFocus(VOICE_KEY_TARGET.field), true);
  assert.equal(peekPendingTarget(), null);
  assert.equal(takePendingFocus(VOICE_KEY_TARGET.field), false);
});
