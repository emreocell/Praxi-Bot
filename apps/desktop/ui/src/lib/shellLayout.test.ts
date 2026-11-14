import assert from "node:assert/strict";
import { test } from "node:test";
import {
  normalizeShellLayout,
  panelMax,
  panelWidthFromPointer,
  railTier,
  railWidthFromPointer,
  RAIL_MINI,
  stepPanel,
  stepRail
} from "./shellLayout.ts";

test("dragging the rail below ~150px collapses it to icons only", () => {
  assert.equal(railWidthFromPointer(149), RAIL_MINI);
  assert.equal(railWidthFromPointer(10), RAIL_MINI);
  assert.equal(railWidthFromPointer(150), 200);
  assert.equal(railWidthFromPointer(320), 320);
  assert.equal(railWidthFromPointer(900), 440);
});

test("the outer gap of the floating layout is subtracted", () => {
  assert.equal(railWidthFromPointer(320, 12), 308);
  assert.equal(railWidthFromPointer(160, 12), RAIL_MINI);
});

test("panel width is measured from the right edge and clamped", () => {
  assert.equal(panelWidthFromPointer(1000, 1400), 400);
  assert.equal(panelWidthFromPointer(1300, 1400), 260);
  assert.equal(panelWidthFromPointer(100, 1400), panelMax(1400));
  assert.equal(panelMax(1400), 840);
  assert.equal(panelMax(300), 260);
});

test("keyboard steps move in 16px and respect the snap", () => {
  assert.equal(stepRail(264, -1), 248);
  assert.equal(stepRail(200, -1), RAIL_MINI);
  assert.equal(stepRail(RAIL_MINI, 1), 200);
  assert.equal(stepRail(440, 1), 440);
  assert.equal(stepPanel(320, 1, 1400), 336);
  assert.equal(stepPanel(260, -1, 1400), 260);
});

test("stored layouts are validated", () => {
  assert.deepEqual(normalizeShellLayout(null), { rail: 264, panel: 320 });
  assert.deepEqual(normalizeShellLayout({ rail: "wide", panel: NaN }), { rail: 264, panel: 320 });
  assert.deepEqual(normalizeShellLayout({ rail: 70, panel: 100 }), { rail: RAIL_MINI, panel: 260 });
  assert.deepEqual(normalizeShellLayout({ rail: 9000, panel: 9000 }), { rail: 440, panel: 1600 });
  assert.deepEqual(normalizeShellLayout({ rail: 310.4, panel: 400 }), { rail: 310, panel: 400 });
});

test("rail tiers decide what the apps pill shows", () => {
  assert.equal(railTier(RAIL_MINI), "mini");
  assert.equal(railTier(200), "narrow");
  assert.equal(railTier(264), "medium");
  assert.equal(railTier(300), "large");
  assert.equal(railTier(379), "large");
  assert.equal(railTier(380), "wide");
  assert.equal(railTier(440), "wide");
});
