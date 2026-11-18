import assert from "node:assert/strict";
import { test } from "node:test";
import { LEGACY_FNS, LEGACY_SIDEBAR_CSS } from "./shellLegacy.ts";
import { MAIN_MIN, PANEL_MAX_FRACTION, PANEL_MIN } from "./shellLayout.ts";

test("the legacy clamp is the share of the window, capped by the room left next to the rail", () => {
  assert.equal(LEGACY_FNS.panelMax(2000, 264, false), Math.round(2000 * PANEL_MAX_FRACTION));
  assert.equal(LEGACY_FNS.panelMax(1000, 264, false), 1000 - 264 - MAIN_MIN);
  assert.equal(LEGACY_FNS.panelMax(300, 264, true), PANEL_MIN);
});

test("the legacy clamp ignores floating chrome and never narrows the rail", () => {
  assert.equal(LEGACY_FNS.panelMax(1200, 264, true), LEGACY_FNS.panelMax(1200, 264, false));
  assert.equal(LEGACY_FNS.fitRail, null);
});

test("the legacy sidebar CSS keeps the inner column as wide as the panel's border box", () => {
  assert.match(LEGACY_SIDEBAR_CSS, /\.panelInner \{ width: var\(--panel-w\); height: 100%; \}/);
  assert.match(LEGACY_SIDEBAR_CSS, /\.panel\[data-open="true"\] \{ width: var\(--panel-w\);/);
});
