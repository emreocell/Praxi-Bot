import assert from "node:assert/strict";
import { test } from "node:test";
import { isTabStop, returnFocusTarget, trapTarget } from "./focusTrap.ts";

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

test("returnFocusTarget prefers a connected previous trigger and falls back when unmounted", () => {
  const connected = new Set(["trigger", "anchor"]);
  assert.equal(
    returnFocusTarget("trigger", (el) => connected.has(el), "anchor"),
    "trigger"
  );
  assert.equal(
    returnFocusTarget("unmounted-menu-item", (el) => connected.has(el), "anchor"),
    "anchor"
  );
  assert.equal(
    returnFocusTarget("unmounted-menu-item", (el) => connected.has(el), "also-gone"),
    null
  );
  assert.equal(returnFocusTarget<string>(null, (el) => connected.has(el)), null);
});
