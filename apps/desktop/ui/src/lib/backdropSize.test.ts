import assert from "node:assert/strict";
import { test } from "node:test";
import { backdropSize, MAX_BACKDROP_PIXELS } from "./backdropCatalog.ts";

test("normal windows use the scene's own divisor", () => {
  assert.deepEqual(backdropSize(1600, 1000, 3), { w: 534, h: 334 });
  assert.deepEqual(backdropSize(1600, 1000, 1.5), { w: 1067, h: 667 });
});

test("huge windows are capped near 2M pixels", () => {
  const { w, h } = backdropSize(7680, 4320, 1.5);
  assert.ok(w * h <= MAX_BACKDROP_PIXELS * 1.01, `${w}x${h}`);
  assert.ok(w * h > MAX_BACKDROP_PIXELS * 0.9);
  const small = backdropSize(3840, 2160, 3);
  assert.ok(small.w * small.h < MAX_BACKDROP_PIXELS);
});

test("degenerate sizes still give a drawable canvas", () => {
  assert.deepEqual(backdropSize(0, 0, 3), { w: 1, h: 1 });
});
