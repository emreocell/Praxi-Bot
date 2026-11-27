import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// The live view's top bar and bottom pill (ux3-live-view-toolbar): fewer controls, labels that
// say what happens.
const viewer = readFileSync(new URL("./LiveViewer.tsx", import.meta.url), "utf8");
const menu = readFileSync(new URL("./LiveViewMenu.tsx", import.meta.url), "utf8");
const en = JSON.parse(readFileSync(new URL("../i18n/locales/en.json", import.meta.url), "utf8"));
const tr = JSON.parse(readFileSync(new URL("../i18n/locales/tr.json", import.meta.url), "utf8"));

const header = viewer.slice(viewer.indexOf("<header"), viewer.indexOf("</header>"));

test("one status pill: the link quality sits inside the state badge, not beside it", () => {
  assert.match(header, /data-testid="live-viewer-state-badge"[\s\S]{0,300}<PeerLinkBadge [^>]*\/>\s*<\/span>/);
  assert.equal(header.match(/<PeerLinkBadge /g)?.length, 1);
  assert.match(viewer, /state\.viewState === "watch"\s*\?\s*t\("liveView\.states\.live"\)/);
});

test("the pop-out lives in the View menu; the bar keeps back, screen, status, full screen, View, stop", () => {
  assert.doesNotMatch(header, /live-viewer-popout/);
  assert.match(menu, /data-testid="live-viewer-popout"/);
  assert.match(header, /data-testid="live-viewer-fullscreen"/);
  assert.match(header, /<LiveViewMenu/);
  assert.match(header, /data-testid="live-viewer-stop"[\s\S]{0,80}title=\{t\("liveView\.emergencyStopHint"\)\}/);
});

test("the keyboard shortcuts are folded in the menu", () => {
  assert.match(menu, /<details className=\{styles\.shortcuts\} data-testid="live-viewer-shortcuts">/);
});

test("labels say what happens, the same words as the computer card", () => {
  assert.equal(tr.liveView.backToChat, "Sohbete dön");
  assert.equal(tr.liveView.watchAction, tr.computerCard.take);
  assert.equal(en.liveView.watchAction, en.computerCard.take);
  assert.equal(en.liveView.emergencyStop, "Stop all bots");
  assert.match(viewer, /t\("liveView\.dock\.watching"\)/);
  assert.match(viewer, /t\("liveView\.dock\.inControl"\)/);
  assert.ok(!/Tamamlandı/.test(tr.liveView.outcomes.none), "the empty outcome is a question, not a claim");
});

test("the local host reads as this computer, not the id 'local'", () => {
  assert.match(viewer, /hostId === "local" \? t\("computerCard\.thisComputer"\)/);
  assert.doesNotMatch(viewer, /· \$\{trg\.host_id\}/);
});
