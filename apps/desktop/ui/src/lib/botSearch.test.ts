import assert from "node:assert/strict";
import { test } from "node:test";
import { filterBots } from "./botSearch.ts";

const bots = [{ name: "İş Asistanı" }, { name: "Mail Helper" }, { name: "Çalışma Notları" }, { name: "Über Bot" }];
const names = (query: string | null | undefined): string[] => filterBots(bots, query).map((b) => b.name);

test("a blank or missing query keeps every bot", () => {
  assert.equal(filterBots(bots, "").length, 4);
  assert.equal(filterBots(bots, "   ").length, 4);
  assert.equal(filterBots(bots, null).length, 4);
  assert.equal(filterBots(bots, undefined).length, 4);
});

test("the Turkish dotted and dotless i are found with a plain i (#638)", () => {
  // "iş" folds to "is", which also occurs in "Çalışma" (calisma): folding is a substring match.
  assert.deepEqual(names("iş"), ["İş Asistanı", "Çalışma Notları"]);
  assert.deepEqual(names("İŞ ASİSTANI"), ["İş Asistanı"]);
  assert.deepEqual(names("asistani"), ["İş Asistanı"]);
  assert.deepEqual(names("ASISTANI"), ["İş Asistanı"]);
});

test("accents are ignored in both directions", () => {
  assert.deepEqual(names("calisma"), ["Çalışma Notları"]);
  assert.deepEqual(names("notlari"), ["Çalışma Notları"]);
  assert.deepEqual(names("uber"), ["Über Bot"]);
  assert.deepEqual(names("Über"), ["Über Bot"]);
});

test("the query is trimmed and every word has to match", () => {
  assert.deepEqual(names("  asistani  "), ["İş Asistanı"]);
  assert.deepEqual(names("asistani is"), ["İş Asistanı"]);
  assert.deepEqual(names("mail asistani"), []);
  assert.deepEqual(names("zzz"), []);
});

test("the order of the bots is kept and extra fields survive", () => {
  const rows = [
    { name: "Alpha", id: "a" },
    { name: "Beta", id: "b" },
    { name: "Alphabet", id: "c" }
  ];
  assert.deepEqual(filterBots(rows, "alpha").map((r) => r.id), ["a", "c"]);
});
