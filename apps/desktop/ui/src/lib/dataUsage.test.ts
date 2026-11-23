import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { DATA_CATEGORY_IDS, formatSize } from "./dataUsage.ts";

test("formatSize picks a readable unit", () => {
  assert.equal(formatSize(0), "0 B");
  assert.equal(formatSize(12 * 1024), "12 KB");
  assert.equal(formatSize(1.5 * 1024 * 1024), "1.5 MB");
  assert.equal(formatSize(2.25 * 1024 ** 3), "2.3 GB");
});

test("every data category has a name and a hint in en, tr and de", () => {
  for (const lang of ["en", "tr", "de"]) {
    const json = JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8"));
    for (const id of DATA_CATEGORY_IDS) {
      assert.ok(json.settingsModal.data.category[id], `${lang}: category ${id}`);
      assert.ok(json.settingsModal.data.categoryHint[id], `${lang}: hint ${id}`);
    }
  }
});
