import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

/**
 * Static guards for wiring that lives in React components and hooks, which the
 * DOM-less test runner cannot mount. They fail if the review-56 fixes are
 * undone in the source; behaviour itself is covered by the pure tests next to
 * them (liveViewStage / liveViewControl / controlBridge).
 */
const read = (rel: string) =>
  readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

const viewer = read("../views/LiveViewer.tsx");
const context = read("../state/LiveViewContext.tsx");
const app = read("../App.tsx");

test("LiveViewer wires key forwarding to one element and guards the local toolbar", () => {
  assert.equal(
    (viewer.match(/onKeyDown=/g) ?? []).length,
    0,
    "no inline key handlers: they come from the stage controller"
  );
  assert.match(viewer, /\{\.\.\.stageController\.ime\}/);
  assert.match(viewer, /\{\.\.\.stageController\.stage\}/);
  assert.match(viewer, /\{\.\.\.stageController\.toolbar\}/);
  // the stage div must not receive the textarea's key handlers
  const stageSpread = viewer.indexOf("{...stageController.stage}");
  const imeSpread = viewer.indexOf("{...stageController.ime}");
  assert.ok(stageSpread > 0 && imeSpread > 0 && stageSpread !== imeSpread);
});

test("LiveViewer locks the target selector while control is held", () => {
  assert.match(viewer, /disabled=\{controlBusy\}/);
});

test("the control bridge registration keeps its identity across control-state refreshes", () => {
  assert.match(context, /createBridgeRegistry\(/);
  assert.match(
    context,
    /registerBridge: \(bridge: ControlBridge \| null\) => void =\s*registry\.register/
  );
  assert.doesNotMatch(context, /bridgeRef/, "no per-render bridge ref");
  assert.doesNotMatch(
    context,
    /registerBridge = useCallback/,
    "registerBridge must not depend on controlState"
  );
});

test("every global Stop call site uses the takeover-only override", () => {
  const calls = app.match(/overrideStop\([^)]*\)/g) ?? [];
  assert.ok(calls.length >= 2, "hotkey and banner button");
  for (const c of calls) {
    assert.match(c, /onlyIfControlled: true/, c);
  }
  assert.match(app, /!event\.repeat/, "key repeat does not re-fire the override");
});

test("the viewer layer sits below dialogs and toasts so approvals stay answerable", () => {
  const z = (css: string, sel: string) => {
    const m = new RegExp(`${sel}\\s*\\{[^}]*?z-index:\\s*(\\d+)`).exec(css);
    assert.ok(m, `z-index of ${sel}`);
    return Number(m[1]);
  };
  const viewerZ = z(read("../views/LiveViewer.module.css"), "\\.backdrop");
  assert.ok(viewerZ < z(read("../components/Modal.module.css"), "\\.backdrop"));
  assert.ok(viewerZ < z(read("../components/Toast.module.css"), "\\.region"));
});
