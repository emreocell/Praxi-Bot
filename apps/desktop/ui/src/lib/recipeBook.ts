import recipeFile from "../data/recipes.json";
import { parseRecipeFile, type Recipe } from "./recipes.ts";

/**
 * The shipped recipes (`data/recipes.json`), validated once at load.  A recipe that fails
 * validation is left out; `recipes.test.ts` fails on it, so it never reaches a release.
 */
export const BOOK = parseRecipeFile(recipeFile);

export function findRecipe(id: string): Recipe | undefined {
  return BOOK.recipes.find((r) => r.id === id);
}
