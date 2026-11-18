import assert from "node:assert/strict";
import { test } from "node:test";
import { foldText, matchesQuery } from "./searchText.ts";

test("foldText lower-cases, strips diacritics and folds the Turkish i", () => {
  assert.equal(foldText("Café"), "cafe");
  assert.equal(foldText("İSTANBUL"), "istanbul");
  assert.equal(foldText("ışık"), "isik");
  assert.equal(foldText("ŞÖĞÜÇ"), "soguc");
});

test("an empty or blank query matches everything", () => {
  assert.equal(matchesQuery("", "anything"), true);
  assert.equal(matchesQuery("   ", undefined), true);
});

test("every word of the query must occur, in any order", () => {
  assert.equal(matchesQuery("daily report", "Report for the daily run"), true);
  assert.equal(matchesQuery("daily missing", "Report for the daily run"), false);
});

test("words may be spread over several texts and nullish texts are skipped", () => {
  assert.equal(matchesQuery("bot weekly", "My bot", null, "weekly digest", undefined), true);
  assert.equal(matchesQuery("x", null, undefined), false);
});

test("matching ignores case, diacritics and the Turkish dotless i", () => {
  assert.equal(matchesQuery("istanbul", "İstanbul planı"), true);
  assert.equal(matchesQuery("PLANI", "plani"), true);
  assert.equal(matchesQuery("cafe", "Café"), true);
});
