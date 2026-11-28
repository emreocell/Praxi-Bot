import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseRecipeFile, type Recipe } from "./recipes.ts";

/** Same wiring as `recipeBook.ts` (JSON import is Vite-only; tests read the file directly). */
const BOOK = parseRecipeFile(
  JSON.parse(readFileSync(new URL("../data/recipes.json", import.meta.url), "utf8"))
);

function findRecipe(id: string): Recipe | undefined {
  return BOOK.recipes.find((r) => r.id === id);
}

test("the shipped recipe book matches recipes.json validation", () => {
  assert.deepEqual(BOOK.errors, []);
  assert.equal(BOOK.recipes.length, 6);
});

test("findRecipe returns a recipe by id and undefined for unknown ids", () => {
  const daily = findRecipe("daily-brief");
  assert.ok(daily);
  assert.equal(daily?.id, "daily-brief");
  assert.equal(findRecipe("not-a-recipe"), undefined);
});