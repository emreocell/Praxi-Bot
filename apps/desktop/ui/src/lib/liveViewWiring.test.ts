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

test("every global Stop call site is one ledger Stop; the takeover override is decided per origin", () => {
  // The hotkey and the run banner make ONE Stop decision through the ledger
  // (native halt + daemon stop + takeover override); App calls nothing else.
  assert.doesNotMatch(app, /overrideStop\(/, "App no longer pauses the takeover itself");
  assert.doesNotMatch(app, /stopBotRun\(/, "nor stops native itself");
  assert.match(app, /if \(!event\.repeat\) \{\s*requestStop\("hotkey"\);/, "one press, never on key repeat");
  assert.match(app, /requestStop\("banner"\)/);
  assert.match(viewer, /requestStop\("viewer"\)/);
  // Behaviour of everything below is tested end to end in stopRuntime.test.ts; these
  // guards only pin that the React host forwards to that runtime and nothing else.
  assert.match(host, /runtime\.setOverride\(\(opts\) => void overrideStop\(opts\)\)/);
  assert.match(host, /onControlStateChanged: \(snap: ControlSnapshot\) => \{\s*runtime\.onNativeSnapshot\(snap\);/);
  assert.match(host, /onDaemonEvent\(\(env\) => runtime\.onDaemonEvent\(env\)\)/);
  assert.match(host, /window\.setInterval\(\(\) => runtime\.tick\(\), HEARTBEAT_MS\)/);
  assert.match(host, /runtime\.attach\(client, bridge\)/);
  assert.match(host, /confirmDriving: \(\) => runtime\.confirmDriving\(\)/);
  assert.match(host, /runtime\.scopeChanged\(\);\s*\}, \[targets, runtime\]\);/);
  assert.doesNotMatch(host, /ledger\.(stopAll|applyList|attachClient|onNativeSnapshot)\(/, "no host logic outside the runtime");
  const chat = read("../views/ChatView.tsx");
  assert.match(chat, /cancelRun\(activeRunId, hard/);
  assert.doesNotMatch(chat, /client\.cancelRun\(/);
  const widget = read("../overlay/WidgetWindow.tsx");
  assert.doesNotMatch(widget, /setControlState/, "every widget snapshot goes through reduceWidget");
  assert.match(widget, /reduceWidget\(prev, failed \? null : snap, performance\.now\(\)\)/);
  assert.match(app, /isNewerNativeSnapshot\(prev\.stateRev \?\? -1, snap\)/);
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

// ---- second review: which element carries which handler, and who calls what ----

const host = read("../state/ControlBridgeHost.tsx");
const modal = read("../components/Modal.tsx");
const palette = read("../components/CommandPalette.tsx");

/** The opening tag (`<div ... >`) that contains `needle`. */
function tagAround(source: string, needle: string): string {
  const at = source.indexOf(needle);
  assert.ok(at > 0, `${needle} present`);
  const open = Math.max(
    source.lastIndexOf("<div", at),
    source.lastIndexOf("<textarea", at),
    source.lastIndexOf("<Button", at)
  );
  return source.slice(open, source.indexOf(">", at) + 1);
}

test("each handler spread sits on the right element", () => {
  const stage = tagAround(viewer, "{...stageController.stage}");
  assert.match(stage, /^<div/);
  assert.match(stage, /ref=\{stageRef\}/);
  assert.doesNotMatch(stage, /stageController\.(ime|toolbar)/);

  const ime = tagAround(viewer, "{...stageController.ime}");
  assert.match(ime, /^<textarea/);
  assert.doesNotMatch(ime, /stageController\.(stage|toolbar)/);

  const toolbar = tagAround(viewer, "{...stageController.toolbar}");
  assert.match(toolbar, /^<div/);
  assert.match(toolbar, /focusOverlayBar/);
  assert.doesNotMatch(toolbar, /stageController\.(stage|ime)/);
});

test("ControlBridgeHost registers its bridge through the stable registerBridge", () => {
  assert.match(host, /registerBridge\(bridge\)/);
  assert.match(host, /registerBridge\(null\)/);
  const deps = /\}, \[([^\]]*)\]\);\s*return null;/.exec(host);
  assert.ok(deps, "effect dependency list");
  assert.match(deps[1], /\bregisterBridge\b/);
  assert.doesNotMatch(deps[1], /controlState/);
  assert.match(host, /onSnapshot: ingestControlSnapshot/);
  assert.equal((host.match(/new ControlBridge\(/g) ?? []).length, 1);
});

test("the context calls the tested helpers (release, close, reconnect)", () => {
  assert.match(context, /await runRelease\(/);
  assert.match(context, /planViewerClose\(stateRef\.current\)/);
  assert.match(context, /runReconnectLoop\(/);
  assert.match(context, /applyStreamDisconnected\(prev\)/);
  assert.match(context, /applySessionCreated\(prev, selectedTarget, session\)/);
  // dismissing never goes through performRelease
  const dismiss = /plan === "dismiss"\) \{[^}]*\}/.exec(context);
  assert.ok(dismiss);
  assert.doesNotMatch(dismiss[0], /performRelease/);
  assert.match(dismiss[0], /setViewerOpen\(false\)/);
  // the old per-attempt deleteViewSession on every drop is gone
  assert.equal(
    (context.match(/deleteViewSession\(/g) ?? []).length,
    4,
    "no-session cleanup, pagehide, effect cleanup, loop delete link"
  );
});

test("the viewer is not aria-modal, never disables its only dismiss, and defers keys to higher layers", () => {
  assert.doesNotMatch(viewer, /aria-modal/);
  const back = tagAround(viewer, 'data-testid="live-viewer-close"');
  assert.doesNotMatch(back, /disabled/);
  assert.match(viewer, /viewerKeyAction\(/);
  assert.doesNotMatch(viewer, /inert/);
});

test("Modal uses the ref-counted inert tracker and the palette is a layer", () => {
  assert.match(modal, /appInert\.acquire\(seq\)/);
  assert.match(modal, /appInert\.release\(seq\)/);
  assert.doesNotMatch(modal, /querySelectorAll\('\[aria-modal/);
  assert.match(palette, /layerStack\.add\(seq\)/);
  assert.match(palette, /e\.stopPropagation\(\)/);
});

// ---- third review ----

const banner = read("../components/PausedControlBanner.tsx");
const panelBlock = read("../components/BotComputerBlock.tsx");

test("a persistent paused-control banner is mounted outside the run banner and offers clear-pause", () => {
  assert.match(app, /<PausedControlBanner \/>/);
  const at = app.indexOf("<PausedControlBanner />");
  const before = app.slice(Math.max(0, at - 120), at);
  assert.doesNotMatch(before, /isRunActive/, "not tied to the native run state Stop resets");
  assert.match(banner, /visibleActions\(/);
  assert.match(banner, /clearPause\("own"\)/);
  assert.match(banner, /openViewer\(\)/);
  assert.doesNotMatch(banner, /controlState\.mode|isRunActive/);
});

test("the panel computer block and the viewer dock both carry reacquire and clear-pause while paused", () => {
  assert.match(panelBlock, /visibleActions\(/);
  assert.match(panelBlock, /bot-computer-reacquire/);
  assert.match(panelBlock, /bot-computer-clear-pause/);
  assert.match(viewer, /live-view-clear-pause/);
  assert.doesNotMatch(viewer, /recoverAction/, "no 'recover & resume' label");
});

test("every non-reacquire paused case clears the latch through POST /control/recover", () => {
  assert.match(context, /prompt\.mode !== "reacquire"\) \{\s*await clearPause\(\)/);
  assert.match(context, /client\.recoverControl\(\)/);
  assert.doesNotMatch(context, /recover: true/, "no takeover-with-recover path any more");
});

test("the command palette is portaled to body and only the topmost layer handles Escape", () => {
  assert.match(palette, /createPortal\(/);
  assert.match(palette, /document\.body/);
  assert.match(palette, /layerStack\.isTop\(seqRef\.current\)/);
  const popover = read("../components/Popover.tsx");
  assert.match(popover, /createPortal\(/, "popovers are portaled too");
});

// ---- fourth review ----

test("every client polls the control snapshot (not only the Tauri bridge) and feeds the banner", () => {
  assert.match(
    context,
    /getControlState\(\)\s*\.then\(\(dto\) => \{\s*if \(clientRef\.current === client\) \{\s*ingestControlSnapshot\(dto, ticket\);/
  );
  assert.match(context, /CONTROL_POLL_MS/);
  const poll = context.slice(context.indexOf("CONTROL_POLL_MS);") - 400, context.indexOf("CONTROL_POLL_MS);") + 40);
  assert.doesNotMatch(poll, /isTauriRuntime/);
});

test("Clear pause goes through the verifying runClearPause and the dock uses the same helper as the banner", () => {
  assert.match(context, /await runClearPause\(/);
  assert.match(viewer, /visibleActions\(state, controlState, isLocalClient\)/);
  assert.match(viewer, /foreignTakeoverOwner\(state\)/);
  assert.match(viewer, /liveView\.connectionLost/);
});
