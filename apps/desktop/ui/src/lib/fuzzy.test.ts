import assert from "node:assert/strict";
import { test } from "node:test";
import { fold, fuzzyMatch, rank } from "./fuzzy.ts";

test("fold keeps the length and ignores case, diacritics and dotless i", () => {
  for (const s of ["Şimdi çalıştır", "İSTANBUL", "ÖĞRENCİ", "Ünlü"]) {
    assert.equal(fold(s).length, s.length);
  }
  assert.equal(fold("Şimdi çalıştır"), "simdi calistir");
  assert.equal(fold("İSTANBUL"), "istanbul");
});

test("fuzzyMatch needs the characters in order", () => {
  assert.ok(fuzzyMatch("nc", "New chat"));
  assert.equal(fuzzyMatch("cn", "New chat"), null);
  assert.equal(fuzzyMatch("xyz", "New chat"), null);
  assert.equal(fuzzyMatch("tahc", "chat"), null);
  assert.equal(fuzzyMatch("longerthantext", "short"), null);
  assert.deepEqual(fuzzyMatch("", "anything"), { score: 0, indices: [] });
});

test("indices point into the original text", () => {
  const m = fuzzyMatch("sc", "Şimdi çalıştır");
  assert.ok(m);
  assert.deepEqual(
    m.indices.map((i) => "Şimdi çalıştır"[i]),
    ["Ş", "ç"]
  );
});

test("Turkish text matches ASCII typing and vice versa", () => {
  assert.ok(fuzzyMatch("calistir", "Şimdi çalıştır"));
  assert.ok(fuzzyMatch("ayarlar", "AYARLAR"));
  assert.ok(fuzzyMatch("istanbul", "İstanbul"));
  assert.ok(fuzzyMatch("çalış", "calistir"));
});

test("prefix beats word-start beats scattered", () => {
  const items = ["Open usage", "Go to settings", "Settings: schedules", "Run now"];
  const r = rank(items, "set", (s) => ({ text: s }));
  assert.deepEqual(
    r.map((x) => x.item),
    ["Settings: schedules", "Go to settings"]
  );
  const r2 = rank(["a big cat", "cat", "concatenate"], "cat", (s) => ({
    text: s
  }));
  assert.deepEqual(
    r2.map((x) => x.item),
    ["cat", "a big cat", "concatenate"]
  );
});

test("consecutive runs beat spread-out matches", () => {
  const r = rank(["s_e_t_t", "settings"], "sett", (s) => ({ text: s }));
  assert.equal(r[0]?.item, "settings");
});

test("empty query keeps order; ties keep order; non-matches drop", () => {
  const items = ["b", "a", "c"];
  assert.deepEqual(
    rank(items, "", (s) => ({ text: s })).map((x) => x.item),
    items
  );
  assert.deepEqual(
    rank(["x1", "x2"], "x", (s) => ({ text: s })).map((x) => x.item),
    ["x1", "x2"]
  );
  assert.deepEqual(
    rank(items, "z", (s) => ({ text: s })),
    []
  );
});

test("keywords match, but rank below a direct title hit", () => {
  const items = [
    { title: "Preferences", kw: ["settings"] },
    { title: "Settings", kw: [] }
  ];
  const r = rank(items, "settings", (i) => ({ text: i.title, keywords: i.kw }));
  assert.deepEqual(
    r.map((x) => x.item.title),
    ["Settings", "Preferences"]
  );
  assert.deepEqual(r[1]?.indices, []);
});
