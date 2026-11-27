import assert from "node:assert/strict";
import { test } from "node:test";
import { appendInstruction } from "./botInstructions.ts";

test("an example fills an empty box and never replaces what the owner wrote", () => {
  assert.equal(appendInstruction("", "Do X."), "Do X.");
  assert.equal(appendInstruction("  \n", "Do X."), "Do X.");
  assert.equal(appendInstruction("Be brief.\n\n", "Do X."), "Be brief.\n\nDo X.");
});
