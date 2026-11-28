import assert from "node:assert/strict";
import { test } from "node:test";
import { needsSearch, rowWindow } from "./panelRows.ts";

const rows = ["code-review", "github-triage", "meeting-notes", "release-notes", "Şirket adresi", "Yazım tercihleri"].map((name) => ({ name }));

test("a list shows its first rows and counts the rest until it is expanded", () => {
  const folded = rowWindow(rows, { query: "", expanded: false, limit: 4 });
  assert.deepEqual(folded.shown.map((r) => r.name), ["code-review", "github-triage", "meeting-notes", "release-notes"]);
  assert.equal(folded.hidden, 2);
  assert.equal(folded.found, 6);
  const open = rowWindow(rows, { query: "", expanded: true, limit: 4 });
  assert.equal(open.shown.length, 6);
  assert.equal(open.hidden, 0);
});

test("a search looks through the folded rows too, ignoring case and Turkish letters", () => {
  const hit = rowWindow(rows, { query: "SIRKET", expanded: false, limit: 2 });
  assert.deepEqual(hit.shown.map((r) => r.name), ["Şirket adresi"]);
  assert.equal(hit.hidden, 0);
  const several = rowWindow(rows, { query: "notes", expanded: false, limit: 1 });
  assert.equal(several.shown.length, 2, "every match shows, not just the first `limit`");
  assert.equal(rowWindow(rows, { query: "zzz", expanded: false, limit: 4 }).found, 0);
  assert.equal(rowWindow(rows, { query: "   ", expanded: false, limit: 4 }).shown.length, 4, "blank is no search");
});

test("the second column is searched as well", () => {
  const docs = [{ name: "Proje belgeleri", meta: "42 files" }, { name: "Sürüm kontrol listesi", meta: "Saved page" }];
  assert.deepEqual(rowWindow(docs, { query: "saved", expanded: false, limit: 4 }).shown.map((r) => r.name), ["Sürüm kontrol listesi"]);
});

test("search appears from the threshold on", () => {
  assert.equal(needsSearch(7, 8), false);
  assert.equal(needsSearch(8, 8), true);
});
