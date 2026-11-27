import assert from "node:assert/strict";
import { test } from "node:test";
import { botDotStatus, railNavIndex } from "./railNav.ts";

test("botDotStatus: working, needs you or idle from the rail's avatar state", () => {
  assert.equal(botDotStatus("working"), "working");
  assert.equal(botDotStatus("thinking"), "working");
  assert.equal(botDotStatus("waiting-approval"), "needsYou");
  assert.equal(botDotStatus("idle"), "idle");
  assert.equal(botDotStatus("error"), "idle");
  assert.equal(botDotStatus(""), "idle");
});

test("railNavIndex: up/down stop at the ends, Home/End jump, other keys are not moves", () => {
  assert.equal(railNavIndex(0, 5, "ArrowDown"), 1);
  assert.equal(railNavIndex(4, 5, "ArrowDown"), 4);
  assert.equal(railNavIndex(0, 5, "ArrowUp"), 0);
  assert.equal(railNavIndex(3, 5, "ArrowUp"), 2);
  assert.equal(railNavIndex(3, 5, "Home"), 0);
  assert.equal(railNavIndex(1, 5, "End"), 4);
  assert.equal(railNavIndex(-1, 5, "ArrowDown"), 0);
  assert.equal(railNavIndex(-1, 5, "ArrowUp"), 4);
  assert.equal(railNavIndex(2, 5, "Enter"), null);
  assert.equal(railNavIndex(0, 0, "ArrowDown"), null);
});
