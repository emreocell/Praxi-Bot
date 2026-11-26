import assert from "node:assert/strict";
import { test } from "node:test";
import { splitInlineCode } from "./inlineCode.ts";

test("a command in backticks becomes a code part and the backticks go away", () => {
  assert.deepEqual(splitInlineCode("Start it with `bot serve --safe` to use safe mode."), [
    { text: "Start it with ", code: false },
    { text: "bot serve --safe", code: true },
    { text: " to use safe mode.", code: false }
  ]);
});

test("several commands, and text that is only a command", () => {
  assert.deepEqual(splitInlineCode("`a` or `b`"), [
    { text: "a", code: true },
    { text: " or ", code: false },
    { text: "b", code: true }
  ]);
  assert.deepEqual(splitInlineCode("`only`"), [{ text: "only", code: true }]);
});

test("plain text, an unpaired backtick and an empty pair stay as they are", () => {
  assert.deepEqual(splitInlineCode("no code here"), [{ text: "no code here", code: false }]);
  assert.deepEqual(splitInlineCode("one ` alone"), [{ text: "one ` alone", code: false }]);
  assert.deepEqual(splitInlineCode("empty `` pair"), [{ text: "empty `` pair", code: false }]);
  assert.deepEqual(splitInlineCode(""), []);
});
