import assert from "node:assert/strict";
import { test } from "node:test";
import { createModalStack } from "./modalStack.ts";

test("the most recently opened dialog is on top, whatever order effects register in", () => {
  const s = createModalStack();
  const first = s.next();
  const second = s.next(); // opened later, e.g. the Themes modal over Settings
  // Both mount in one commit; the child-first effect order registers `second` before `first`.
  s.add(second);
  s.add(first);
  assert.equal(s.isTop(second), true);
  assert.equal(s.isTop(first), false);
});

test("closing the top dialog hands the top to the one below", () => {
  const s = createModalStack();
  const a = s.next();
  const b = s.next();
  s.add(a);
  s.add(b);
  s.remove(b);
  assert.equal(s.isTop(a), true);
  assert.equal(s.isTop(b), false);
  s.remove(a);
  assert.equal(s.isTop(a), false);
});

test("sequence numbers only grow, also after dialogs close", () => {
  const s = createModalStack();
  const a = s.next();
  s.add(a);
  s.remove(a);
  assert.ok(s.next() > a);
});
