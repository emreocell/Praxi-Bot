import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { buildGuideIndex, guideForLanguage, indexPage, plainMarkdown } from "./guideIndex.ts";
import type { GuideEntry } from "./guideIndex.ts";
import { searchGuide } from "./guideIndex.ts";

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

for (const lang of ["tr", "de"]) {
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
