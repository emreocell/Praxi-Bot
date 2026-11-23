// Run with: npm test (Node 22.6+ runs the .ts helper directly).
import { test } from "node:test";
import assert from "node:assert/strict";
import { switchTarget, currentLang } from "../.vitepress/theme/langs.ts";

const translated = { tr: ["getting-started"], de: ["getting-started", "faq"] };

test("keeps the page when the translation exists", () => {
  assert.equal(switchTarget("guide/getting-started.md", "tr", translated), "guide/tr/getting-started");
  assert.equal(switchTarget("guide/de/faq.md", "en", translated), "guide/faq");
  assert.equal(switchTarget("guide/tr/getting-started.md", "de", translated), "guide/de/getting-started");
});

test("falls back to English for an untranslated page, and to the guide home outside the guide", () => {
  assert.equal(switchTarget("guide/faq.md", "tr", translated), "guide/faq");
  assert.equal(switchTarget("contributing/docs-site.md", "de", translated), "guide/de/index");
  assert.equal(currentLang("guide/tr/index.md"), "tr");
  assert.equal(currentLang("guide/faq.md"), "en");
});
