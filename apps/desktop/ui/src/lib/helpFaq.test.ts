import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";

type Faq = typeof import("./helpFaq.ts");
type Item = import("./helpFaq.ts").FaqItem;

let server: ViteDevServer;
let faq: Faq;
let en: readonly Item[];
let tr: readonly Item[];
let de: readonly Item[];

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null }
  });
  faq = (await server.ssrLoadModule("/src/lib/helpFaq.ts")) as Faq;
  en = faq.faqFor("en");
  tr = faq.faqFor("tr");
  de = faq.faqFor("de");
});

after(async () => {
  await server?.close();
});

const GUIDE = fileURLToPath(new URL("../../../../../docs/guide/", import.meta.url));

test("the help answers cover the same 15 questions in en, tr and de", () => {
  assert.equal(en.length, 15);
  assert.deepEqual(tr.map((i) => i.id), en.map((i) => i.id));
  assert.deepEqual(de.map((i) => i.id), en.map((i) => i.id));
  for (const [i, item] of en.entries()) {
    for (const other of [tr[i], de[i]]) {
      assert.notEqual(other.q, item.q, `${item.id} question is still English`);
      assert.notEqual(other.a, item.a, `${item.id} answer is still English`);
      assert.equal(other.doc, item.doc);
    }
  }
});

test("answers are short, jargon-free and link to a docs page that exists", () => {
  const jargon = /\b(daemon|peers?|egress|taint|payload|endpoint|sandbox|token|json|http|cli|mcp|api (?:path|url))\b|\bDaemon\b|\bEndpunkt\b/i;
  for (const list of [en, tr, de]) {
    for (const item of list) {
      assert.ok(item.a.length > 40 && item.a.length < 420, `${item.id} answer length`);
      assert.ok(!jargon.test(item.q) && !jargon.test(item.a.replace(/API-?(?:key|Schlüssel|anahtar)\w*/gi, "")), `${item.id} jargon`);
      assert.ok(existsSync(`${GUIDE}${item.doc}`), `docs/guide/${item.doc} exists`);
      assert.match(faq.docUrl(item), /^https:\/\/github\.com\/emreocell\/Praxi-Bot\/blob\/main\/docs\/guide\/.+\.md$/);
    }
  }
});

test("search finds answers by any words, ignores case and accents, and falls back to English", () => {
  assert.deepEqual(faq.searchFaq(en, "free KEY").map((i) => i.id).slice(0, 1), ["freeKey"]);
  assert.ok(faq.searchFaq(de, "kostet").some((i) => i.id === "cost"));
  assert.ok(faq.searchFaq(tr, "ucretsiz anahtar").some((i) => i.id === "freeKey"));
  assert.equal(faq.searchFaq(en, "   ").length, 15);
  assert.equal(faq.searchFaq(en, "zzzzqqq").length, 0);
  assert.equal(faq.faqFor("fr-FR"), faq.faqFor("en"));
  assert.equal(faq.faqFor("tr-TR")[0].q, tr[0].q);
});
