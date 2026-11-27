import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { buildGuideIndex, guideForLanguage, indexPage, plainMarkdown } from "./guideIndex.ts";
import type { GuideEntry } from "./guideIndex.ts";
import { containsFolded, foldVariants, queryWords, searchGuide } from "./guideIndex.ts";

const GUIDE_INDEX: GuideEntry[] = JSON.parse(
  readFileSync(new URL("../help/guideIndex.json", import.meta.url), "utf8")
);

test("plainMarkdown drops links, emphasis and code ticks but keeps snake_case names", () => {
  assert.equal(plainMarkdown("See **the [guide](a.md)** and `notes_read`."), "See the guide and notes_read.");
  assert.equal(plainMarkdown("_soft_ <b>tag</b>"), "soft tag");
});

test("indexPage reads the title, headings (not code) and the first plain sentence", () => {
  const e = indexPage(
    "x.md",
    [
      "# My page",
      "",
      "| a | b |",
      "",
      "Opening **sentence** with a [link](y.md).",
      "",
      "## First",
      "```sh",
      "# not a heading",
      "```",
      "### Second `code`",
      "#### too deep"
    ].join("\n")
  );
  assert.equal(e.title, "My page");
  assert.deepEqual(e.headings, ["First", "Second code"]);
  assert.equal(e.summary, "Opening sentence with a link.");
});

test("indexPage falls back to the file name and truncates long summaries", () => {
  assert.equal(indexPage("plain-name.md", "no heading here").title, "plain-name");
  const long = indexPage("l.md", `# T\n\n${"word ".repeat(80)}`);
  assert.ok(long.summary.length <= 200);
  assert.ok(long.summary.endsWith("…"));
});

test("buildGuideIndex sorts by file name and ignores other files", () => {
  const out = buildGuideIndex([
    { name: "b.md", text: "# B" },
    { name: "a.md", text: "# A" },
    { name: "c.txt", text: "# C" }
  ]);
  assert.deepEqual(
    out.map((e) => e.doc),
    ["a.md", "b.md"]
  );
});

test("the committed guide index matches docs/guide (run build:guide-index after editing the guide)", () => {
  const dir = fileURLToPath(new URL("../../../../../docs/guide/", import.meta.url));
  const files = readdirSync(dir)
    .filter((n) => n.endsWith(".md"))
    .map((name) => ({ name, text: readFileSync(dir + name, "utf8") }));
  assert.deepEqual(GUIDE_INDEX, buildGuideIndex(files));
});

test("searchGuide needs every word, ranks title over heading over summary, and caps results", () => {
  const index = [
    { doc: "s.md", title: "Other", headings: [], summary: "mentions backup once" },
    { doc: "h.md", title: "Other two", headings: ["Backup now"], summary: "" },
    { doc: "t.md", title: "Backup and restore", headings: [], summary: "" }
  ];
  assert.deepEqual(
    searchGuide(index, "BACKUP").map((e) => e.doc),
    ["t.md", "h.md", "s.md"]
  );
  assert.deepEqual(searchGuide(index, "backup restore").map((e) => e.doc), ["t.md"]);
  assert.equal(searchGuide(index, "backup", 2).length, 2);
  assert.deepEqual(searchGuide(index, "   "), []);
  assert.deepEqual(searchGuide(index, "zzzz"), []);
});

test("the real index finds the guide pages a beginner would look for", () => {
  assert.ok(searchGuide(GUIDE_INDEX, "backup").some((e) => e.doc.startsWith("backup")));
  assert.ok(searchGuide(GUIDE_INDEX, "telegram").some((e) => e.doc === "telegram.md"));
});

const GUIDE_DIR = fileURLToPath(new URL("../../../../../docs/guide/", import.meta.url));
/** Language folders under docs/guide (the same rule the generator uses). */
const GUIDE_LANGUAGES = readdirSync(GUIDE_DIR, { withFileTypes: true })
  .filter((d) => d.isDirectory() && readdirSync(GUIDE_DIR + d.name).some((n) => n.endsWith(".md")))
  .map((d) => d.name);

test("docs/guide has the translated folders the app knows about", () => {
  assert.ok(GUIDE_LANGUAGES.includes("tr") && GUIDE_LANGUAGES.includes("de"));
});

for (const lang of GUIDE_LANGUAGES) {
  test(`the committed ${lang} guide index matches docs/guide/${lang}`, () => {
    const dir = fileURLToPath(new URL(`../../../../../docs/guide/${lang}/`, import.meta.url));
    const files = readdirSync(dir)
      .filter((n) => n.endsWith(".md"))
      .map((name) => ({ name: `${lang}/${name}`, text: readFileSync(dir + name, "utf8") }));
    const committed: GuideEntry[] = JSON.parse(
      readFileSync(new URL(`../help/guideIndex.${lang}.json`, import.meta.url), "utf8")
    );
    assert.deepEqual(committed, buildGuideIndex(files));
    // Every translated page has an English original, so the in-app link never dangles.
    const english = new Set(GUIDE_INDEX.map((e) => e.doc));
    for (const e of committed) {
      assert.ok(english.has(e.doc.slice(lang.length + 1)), `${e.doc} has an English original`);
    }
  });
}

test("guideForLanguage lists translated pages and falls back to English for the rest", () => {
  const en = [
    { doc: "a.md", title: "Backup", headings: [], summary: "" },
    { doc: "b.md", title: "Skills", headings: [], summary: "" }
  ];
  const tr = [{ doc: "tr/a.md", title: "Yedekleme", headings: [], summary: "" }];
  const merged = guideForLanguage(en, tr);
  assert.deepEqual(
    merged.map((e) => e.doc),
    ["tr/a.md", "b.md"]
  );
  assert.deepEqual(searchGuide(merged, "yedekleme").map((e) => e.doc), ["tr/a.md"]);
  assert.deepEqual(searchGuide(merged, "skills").map((e) => e.doc), ["b.md"]);
});

const find = (index: readonly GuideEntry[], query: string, language: string) =>
  searchGuide(index, query, 10, language).map((e) => e.doc);

test("foldVariants: Turkish dotted and dotless i fold together, whatever the OS locale", () => {
  for (const word of ["ISI", "ısı", "isi", "İSİ", "Isı"]) {
    assert.deepEqual(foldVariants(word, "tr"), ["isi"], word);
  }
  assert.deepEqual(foldVariants("Çalışma ĞÜŞÖ", "tr"), ["calisma guso"]);
  // A decomposed "İ" (I + combining dot) folds the same as the single letter.
  assert.deepEqual(foldVariants("İndir", "tr"), foldVariants("İndir", "tr"));
});

test("foldVariants: German ß and umlauts match with or without the special keys", () => {
  assert.deepEqual(foldVariants("Größe Straße", "de"), ["grosse strasse", "groesse strasse"]);
  assert.deepEqual(foldVariants("für", "de-DE"), ["fur", "fuer"]);
  // Other languages keep a single form, so English text is not rewritten.
  assert.deepEqual(foldVariants("für", "en"), ["fur"]);
  assert.deepEqual(foldVariants("Größe"), ["grosse"]);
});

test("queryWords splits on punctuation and keeps decomposed letters together", () => {
  assert.deepEqual(queryWords("  e-mail, Wi-Fi?  ").map((w) => w[0]), ["e", "mail", "wi", "fi"]);
  assert.deepEqual(queryWords("über", "de"), [["uber", "ueber"]]);
  assert.deepEqual(queryWords("???"), []);
});

test("containsFolded compares form by form", () => {
  assert.equal(containsFolded(["fur sie", "fuer sie"], ["fuer", "fuer"]), true);
  assert.equal(containsFolded(["fur sie"], ["fuer", "fuer"]), false);
  assert.equal(containsFolded([], ["a"]), false);
});

test("searchGuide in Turkish finds pages however the user types i, ı and the accented letters", () => {
  const tr: GuideEntry[] = [
    { doc: "tr/a.md", title: "Işık ve ısı ayarları", headings: ["Çalışma saatleri"], summary: "" },
    { doc: "tr/b.md", title: "Diğer", headings: [], summary: "İndirme klasörü" }
  ];
  for (const q of ["ISIK", "ışık", "isik", "ISI ayarlari"]) {
    assert.deepEqual(find(tr, q, "tr"), ["tr/a.md"], q);
  }
  assert.deepEqual(find(tr, "calisma", "tr-TR"), ["tr/a.md"]);
  assert.deepEqual(find(tr, "INDIRME", "tr"), ["tr/b.md"]);
  assert.deepEqual(find(tr, "İNDİRME", "tr"), ["tr/b.md"]);
  assert.deepEqual(find(tr, "diger", "tr"), ["tr/b.md"]);
});

test("searchGuide in German finds umlaut and ß pages with ä/ö/ü, a/o/u, ae/oe/ue and ss", () => {
  const de: GuideEntry[] = [
    { doc: "de/a.md", title: "Größe der Übersicht", headings: ["Für Anfänger"], summary: "Straße" },
    { doc: "de/b.md", title: "Backup", headings: [], summary: "Sicherung" }
  ];
  for (const q of ["Übersicht", "ubersicht", "uebersicht", "UEBERSICHT", "größe", "grosse", "groesse", "GROSSE"]) {
    assert.deepEqual(find(de, q, "de"), ["de/a.md"], q);
  }
  assert.deepEqual(find(de, "fuer anfaenger", "de"), ["de/a.md"]);
  assert.deepEqual(find(de, "für anfänger", "de-AT"), ["de/a.md"]);
  assert.deepEqual(find(de, "strasse", "de"), ["de/a.md"]);
  // Without a language only the accent-stripped form applies, as before.
  assert.deepEqual(find(de, "uebersicht", ""), []);
  assert.deepEqual(find(de, "ubersicht", ""), ["de/a.md"]);
});

test("searchGuide ignores punctuation in the query and still needs every word", () => {
  const index = [{ doc: "m.md", title: "E-mail setup", headings: [], summary: "" }];
  assert.deepEqual(find(index, "e-mail,", "en"), ["m.md"]);
  assert.deepEqual(find(index, "email", "en"), []);
  assert.deepEqual(find(index, "mail telegram", "en"), []);
});
