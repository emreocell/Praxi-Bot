import assert from "node:assert/strict";
import { test } from "node:test";
import { parseThinkingLevel, THINKING_LEVELS } from "./thinkingLevel.ts";

test("only known levels are accepted, anything else is medium", () => {
  for (const l of THINKING_LEVELS) assert.equal(parseThinkingLevel(l), l);
  assert.equal(parseThinkingLevel("extreme"), "medium");
  assert.equal(parseThinkingLevel(null), "medium");
  assert.equal(parseThinkingLevel(3), "medium");
});
