import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { APP_FNS, evaluateShell, shellBoxes } from "./shellGeometry.ts";
import { MUTATIONS } from "./shellMutations.ts";
import { LEGACY_FNS, LEGACY_SIDEBAR_CSS } from "./shellLegacy.ts";
import { evalLength, readShellContract } from "./shellContract.ts";
import { FLOAT_GAP, FLOAT_PAD } from "./shellLayout.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const sources = {
  sidebarTsx: read("../../../../../packages/ui/src/components/Sidebar.tsx"),
  sidebarCss: read("../../../../../packages/ui/src/components/Sidebar.module.css"),
  tokensCss: read("../../../../../packages/ui/tokens.css"),
  globalCss: read("../global.css"),
  themeTs: read("../../../../../packages/ui/src/theme.ts")
};

test("the shipped shell passes the contract and the whole matrix", () => {
  const report = evaluateShell(sources, APP_FNS);
  assert.deepEqual(report.contractFailures, []);
  assert.deepEqual(report.violations, []);
  assert.ok(report.cells >= 600);
  assert.ok(report.narrowestMain.width >= 280);
});

test("the pre-fix CSS and clamp violate the clip and chat-width invariants", () => {
  const legacy = evaluateShell({ ...sources, sidebarCss: LEGACY_SIDEBAR_CSS }, LEGACY_FNS);
  const text = [...legacy.contractFailures, ...legacy.violations].join("\n");
  assert.match(text, /\.panelInner width/);
  assert.match(text, /panelInner\.right \d+ > panel content right \d+/);
  assert.match(text, /vw=760 .*main [\d.]+px < 280/);
});

test("the CSS contract alone catches an inner column as wide as the panel's border box", () => {
  const css = sources.sidebarCss.replace(/width: calc\(var\(--panel-w\) - var\(--panel-edge, var\(--space-0\)\)\);/, "width: var(--panel-w);");
  assert.notEqual(css, sources.sidebarCss);
  const report = evaluateShell({ ...sources, sidebarCss: css }, APP_FNS);
  assert.ok(report.contractFailures.some((f) => f.includes(".panelInner width")));
  assert.ok(report.violations.some((v) => v.includes("clipped")));
});

test("a wrong panel-edge value is a geometry violation even when the width rule looks fine", () => {
  const css = sources.sidebarCss.replace("--panel-edge: calc(var(--size-border) * 2);", "--panel-edge: var(--size-border);");
  assert.notEqual(css, sources.sidebarCss);
  const report = evaluateShell({ ...sources, sidebarCss: css }, APP_FNS);
  assert.deepEqual(report.contractFailures, []);
  assert.ok(report.violations.some((v) => v.includes("clipped 1px")));
});

test("boxes of the reported case: 1400 px, float, panel open", () => {
  const { model } = readShellContract(sources, { floatPad: FLOAT_PAD, floatGap: FLOAT_GAP });
  const g = shellBoxes({ vw: 1400, rail: 264, panel: 320, panelOpen: true, float: true }, model);
  assert.deepEqual(g.rail, { left: 12, right: 276 });
  assert.deepEqual(g.panel, { left: 1068, right: 1388 });
  assert.equal(g.panelContentRight, 1387);
  assert.equal(g.panelInner.right, 1387);
  assert.equal(g.main.right - g.main.left, 1068 - 12 - 288);
});

test("the length evaluator reads plain CSS lengths", () => {
  const vars = new Map([["--space-3", "12px"]]);
  assert.equal(evalLength("var(--space-3)", vars), 12);
  assert.equal(evalLength("calc(var(--space-3) * -1)", vars), -12);
  assert.equal(evalLength("calc(320px - var(--missing, 2px))", vars), 318);
  assert.equal(evalLength("calc(320px - var(--missing, var(--space-3)))", vars), 308);
  assert.equal(evalLength("100%", vars), undefined);
});

for (const m of MUTATIONS) {
  test(`single mutation is rejected on its own: ${m.name}`, () => {
    const css = m.css ? m.css(sources.sidebarCss) : sources.sidebarCss;
    const report = evaluateShell({ ...sources, sidebarCss: css }, APP_FNS, m.options);
    const text = [...report.contractFailures, ...report.violations].join("\n");
    assert.ok(text.length > 0, "the check must fail");
    assert.match(text, m.expect);
  });
}

test("the rail reserve keeps the chat above its minimum at every frame of the close slide", () => {
  const settled = evaluateShell(sources, APP_FNS, { railPolicy: "settled" });
  assert.deepEqual(settled.violations, []);
  const immediate = evaluateShell(sources, APP_FNS, { railPolicy: "immediate" });
  // Review numbers: 760 px, float, stored rail 440, panel 260: the chat is squeezed while the panel still slides.
  assert.ok(immediate.violations.some((v) => /vw=760 rail=440 panel=260 float=on closing t=0ms progress=1\.00 \(transition\): main 4\d(\.\d+)?px/.test(v)));
});

test("overlay panel keeps main column width identical to closed state", () => {
  const { model } = readShellContract(sources, { floatPad: FLOAT_PAD, floatGap: FLOAT_GAP });
  const closed = shellBoxes({ vw: 1280, rail: 264, panel: 320, panelOpen: false, float: true }, model);
  const overlay = shellBoxes({ vw: 1280, rail: 264, panel: 320, panelOpen: true, panelMode: "overlay", float: true }, model);
  assert.equal(overlay.main.left, closed.main.left);
  assert.equal(overlay.main.right, closed.main.right);
  assert.equal(overlay.panel.right, 1280 - FLOAT_PAD);
});
