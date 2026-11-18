import assert from "node:assert/strict";
import { test } from "node:test";
import { isTabStop, trapTarget } from "./focusTrap.ts";

test("a tab stop needs a non-negative tabIndex, a layout box and no inert ancestor", () => {
  assert.equal(isTabStop({ tabIndex: 0, rects: 1 }), true);
  assert.equal(isTabStop({ tabIndex: -1, rects: 1 }), false);
  assert.equal(isTabStop({ tabIndex: 0, rects: 0 }), false);
  assert.equal(isTabStop({ tabIndex: 0, rects: 1, inert: true }), false);
});

const items = ["a", "b", "c"];

test("Tab on the last stop wraps to the first; elsewhere the browser moves on", () => {
  assert.equal(trapTarget(items, "c", "dlg", false), "a");
  assert.equal(trapTarget(items, "b", "dlg", false), null);
  assert.equal(trapTarget(items, "a", "dlg", false), null);
});

test("Shift+Tab on the first stop (or the dialog) wraps to the last", () => {
  assert.equal(trapTarget(items, "a", "dlg", true), "c");
  assert.equal(trapTarget(items, "dlg", "dlg", true), "c");
  assert.equal(trapTarget(items, "b", "dlg", true), null);
});

test("focus outside the cycle is pulled back in", () => {
  assert.equal(trapTarget(items, "x", "dlg", false), "a");
  assert.equal(trapTarget(items, null, "dlg", false), "a");
  assert.equal(trapTarget(items, "x", "dlg", true), "c");
});

test("with no stops the caller keeps focus on the dialog", () => {
  assert.equal(trapTarget([], null, "dlg", false), "none");
  assert.equal(trapTarget([], "dlg", "dlg", true), "none");
});
