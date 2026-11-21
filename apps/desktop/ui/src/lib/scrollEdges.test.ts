import assert from "node:assert/strict";
import { test } from "node:test";
import { scrollEdges } from "./scrollEdges.ts";

test("content that fits shows no edge shadow", () => {
  assert.deepEqual(scrollEdges(0, 400, 400), { top: false, bottom: false });
  assert.deepEqual(scrollEdges(0, 400, 400.6), { top: false, bottom: false });
});

test("the shadow follows where hidden content is", () => {
  assert.deepEqual(scrollEdges(0, 400, 900), { top: false, bottom: true });
  assert.deepEqual(scrollEdges(200, 400, 900), { top: true, bottom: true });
  assert.deepEqual(scrollEdges(500, 400, 900), { top: true, bottom: false });
  // a fractional rest position at the very end still counts as the end
  assert.deepEqual(scrollEdges(499.5, 400, 900), { top: true, bottom: false });
});
