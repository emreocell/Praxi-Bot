import assert from "node:assert/strict";
import { existsSync, readdirSync } from "node:fs";
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

test("the Help window offers the seven plain-words start pages in order, translated in tr and de", () => {
  const names = (lang: string) => faq.startPages(lang).map((p) => p.doc.slice(p.doc.lastIndexOf("/") + 1));
  for (const lang of ["en", "tr", "de"]) {
    assert.deepEqual(names(lang), [...faq.START_PAGES], `${lang} start pages`);
    for (const page of faq.startPages(lang)) {
      assert.ok(existsSync(`${GUIDE}${page.doc}`), `docs/guide/${page.doc} exists`);
      assert.equal(page.doc.startsWith(`${lang}/`), lang !== "en", `${page.doc} is in ${lang}`);
    }
  }
});

test("FAQ search is language-aware: Turkish i/ı and German ß/ae/oe/ue", () => {
  const ids = (items: readonly Item[], q: string, lang: string) => faq.searchFaq(items, q, lang).map((i) => i.id);
  // "Bir botu hemen nasıl durdururum?" typed with a dotted i, with capitals, and with the dotless one.
  for (const q of ["nasil", "NASIL", "nasıl", "NASIL durdururum"]) {
    assert.ok(ids(tr, q, "tr").length > 0, q);
  }
  assert.ok(ids(tr, "ucretsiz", "tr").includes("freeKey"));
  assert.ok(ids(tr, "ÜCRETSİZ", "tr").includes("freeKey"));
  // "Kann ein Bot meine Dateien löschen?" and "Schlüssel" without umlauts, with ae/oe/ue, and with them.
  for (const q of ["loschen", "loeschen", "löschen", "LÖSCHEN"]) {
    assert.ok(ids(de, q, "de-DE").length > 0, q);
  }
  assert.ok(ids(de, "schluessel", "de").includes("freeKey"));
  assert.ok(ids(de, "schlussel", "de").includes("freeKey"));
  assert.ok(ids(de, "Schlüssel", "de").includes("freeKey"));
  // Punctuation in the query does not block a match, and an empty query still lists everything.
  assert.ok(ids(de, "kostet das?", "de").includes("cost"));
  assert.equal(faq.searchFaq(de, "  ", "de").length, 15);
});

test("every language folder in docs/guide has a registered index, and nothing else is registered", () => {
  const folders = readdirSync(GUIDE, { withFileTypes: true })
    .filter((d) => d.isDirectory() && readdirSync(`${GUIDE}${d.name}`).some((n) => n.endsWith(".md")))
    .map((d) => d.name)
    .sort();
  assert.deepEqual([...faq.GUIDE_LANGUAGES].sort(), folders);
});

test("a language searches its own pages first and the English page for anything not translated yet", () => {
  const basename = (doc: string) => doc.slice(doc.lastIndexOf("/") + 1);
  const english = new Set(faq.GUIDE_INDEX.map((e) => e.doc));
  for (const lang of faq.GUIDE_LANGUAGES) {
    const pages = faq.guideFor(lang);
    // Nothing is lost: every English page is reachable, once, in its own language or in English.
    assert.deepEqual(pages.map((p) => basename(p.doc)).sort(), [...english].sort(), `${lang} covers the whole guide`);
    for (const page of pages) {
      const inLang = page.doc.startsWith(`${lang}/`);
      assert.ok(existsSync(`${GUIDE}${page.doc}`), `docs/guide/${page.doc} exists`);
      assert.equal(faq.guideUrl(page), `${faq.DOCS_BASE_URL}${page.doc}`);
      // A page is the translation when one exists, else the English original.
      assert.equal(inLang, existsSync(`${GUIDE}${lang}/${basename(page.doc)}`), `${page.doc} is the right language`);
    }
  }
  // A language without an index searches English; a region tag still picks the language.
  assert.equal(faq.guideFor("fr"), faq.GUIDE_INDEX);
  assert.equal(faq.guideFor("de-AT"), faq.guideFor("de"));
});

test("searching the guide in German and Turkish opens the page in that language", () => {
  const first = (lang: string, q: string) => faq.searchGuide(faq.guideFor(lang), q, 5, lang)[0];
  assert.equal(first("de", "Einstellungen")?.doc, "de/settings.md");
  assert.equal(first("tr", "ayarlar")?.doc, "tr/settings.md");
  assert.match(faq.guideUrl(first("de", "Einstellungen")), /\/docs\/guide\/de\/settings\.md$/);
  // Not translated in German yet: the English page is returned, so the link still works.
  // (Once German is complete there is nothing left to fall back to and this check has no subject.)
  const untranslated = faq.GUIDE_INDEX.find((e) => !existsSync(`${GUIDE}de/${e.doc}`));
  if (untranslated) {
    assert.ok(faq.searchGuide(faq.guideFor("de"), untranslated.title, 10, "de").some((e) => e.doc === untranslated.doc));
  }
});
