import assert from "node:assert/strict";
import { test } from "node:test";
import { validateProbe, type ProbeBox, type ProbeData } from "./layoutProbeCheck.ts";

const box = (left: number, right: number): ProbeBox => ({ left, right, top: 12, bottom: 880, width: right - left });
const good = (): ProbeData => ({
  viewport: { width: 1400, height: 900 },
  float: "on",
  style: "glass",
  theme: "clay",
  corners: "soft",
  backdrop: "off",
  container: box(0, 1400),
  rail: box(12, 276),
  main: box(288, 1038),
  panel: box(1050, 1388),
  panelOpenAttr: "true",
  panelBorders: { left: 1, right: 1, top: 1, bottom: 1 },
  panelContentRight: 1387,
  panelInner: box(1051, 1387),
  panelInnerScroll: { scrollWidth: 336, clientWidth: 336 },
  panelDescendants: 12,
  panelRightMostDescendant: { tag: "div", cls: "x", right: 1380 },
  document: { scrollWidth: 1400, clientWidth: 1400 }
});
const open = { open: true, theme: "clay", style: "glass" };

test("a complete, correct probe passes", () => {
  assert.deepEqual(validateProbe(good(), open), []);
});

test("a missing shell is a failure, never a pass", () => {
  const p = good();
  p.rail = null;
  p.main = null;
  p.panel = null;
  p.panelInner = null;
  assert.ok(validateProbe(p, open).some((v) => /required rail missing/.test(v)));
  assert.ok(validateProbe(p, open).some((v) => /required panelInner missing/.test(v)));
  assert.deepEqual(validateProbe(null, open), ["no probe data"]);
  assert.match(validateProbe({ ...good(), error: "not ready after 15000 ms: panel" }, open)[0], /did not measure/);
});

test("zero-size, non-finite, wrong state, wrong appearance and an empty panel fail", () => {
  const zero = good();
  zero.panelInner = { ...box(1051, 1051) };
  assert.ok(validateProbe(zero, open).some((v) => /panelInner missing, non-finite or zero-size/.test(v)));
  const nan = good();
  nan.main = { ...box(288, 1038), left: Number.NaN };
  assert.ok(validateProbe(nan, open).some((v) => /required main/.test(v)));
  const closed = good();
  closed.panelOpenAttr = "false";
  assert.ok(validateProbe(closed, open).some((v) => /open state false != expected true/.test(v)));
  assert.ok(validateProbe(good(), { ...open, theme: "obsidian" }).some((v) => /theme clay != expected obsidian/.test(v)));
  const empty = good();
  empty.panelDescendants = 0;
  assert.ok(validateProbe(empty, open).some((v) => /no rendered content/.test(v)));
});

test("the measured clip of the original bug is still reported", () => {
  const p = good();
  p.panelInner = box(1051, 1389);
  p.panelContentRight = 1387;
  assert.ok(validateProbe(p, open).some((v) => /clipped 2\.0px/.test(v)));
});

test("a closed expectation does not need the panel boxes", () => {
  const p = good();
  p.panelOpenAttr = "false";
  p.panel = null;
  p.panelInner = null;
  p.panelDescendants = 0;
  assert.deepEqual(validateProbe(p, { open: false, theme: "clay" }), []);
});

test("an overlay panel may cover the right edge of main without failing overlap, unless it overlaps rail", () => {
  const p = good();
  p.panelMode = "overlay";
  p.main = box(288, 1388);
  assert.deepEqual(validateProbe(p, open), []);
  p.panel = box(200, 1388);
  assert.ok(validateProbe(p, open).some((v) => /overlay panel overlaps rail/.test(v)));
});
