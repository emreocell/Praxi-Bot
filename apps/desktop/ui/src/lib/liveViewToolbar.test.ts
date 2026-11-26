import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  toggleFit,
  viewerAnnouncementKey,
  VIEWER_SHORTCUTS,
  viewerShortcut,
  type ShortcutEvent
} from "./liveViewToolbar.ts";

const ev = (over: Partial<ShortcutEvent>): ShortcutEvent => ({
  key: "",
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  defaultPrevented: false,
  editable: false,
  ...over
});
const watching = { interactive: false, stageFocused: false };

test("F and Ctrl+Shift+D map to their actions while watching", () => {
  assert.equal(viewerShortcut(ev({ key: "f" }), watching), "fit");
  assert.equal(viewerShortcut(ev({ key: "F" }), watching), "fit");
  assert.equal(viewerShortcut(ev({ key: "D", ctrlKey: true, shiftKey: true }), watching), "takeover");
  assert.equal(viewerShortcut(ev({ key: "?", shiftKey: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: "f", ctrlKey: true }), watching), null);
});

test("F11 toggles full screen and 0, 1, + and - set the screen's size while watching", () => {
  assert.equal(viewerShortcut(ev({ key: "F11" }), watching), "fullscreen");
  assert.equal(viewerShortcut(ev({ key: "0" }), watching), "zoomFit");
  assert.equal(viewerShortcut(ev({ key: "1" }), watching), "zoomActual");
  assert.equal(viewerShortcut(ev({ key: "+", shiftKey: true }), watching), "zoomIn");
  assert.equal(viewerShortcut(ev({ key: "=" }), watching), "zoomIn");
  assert.equal(viewerShortcut(ev({ key: "-" }), watching), "zoomOut");
  assert.equal(viewerShortcut(ev({ key: "1", ctrlKey: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: "F11", altKey: true }), watching), null);
  const driving = { interactive: true, stageFocused: false };
  assert.equal(viewerShortcut(ev({ key: "1" }), driving), null);
  assert.equal(viewerShortcut(ev({ key: "F11" }), driving), null);
});

test("the shifted forms of + and - zoom, and Shift does not hide a digit typed with it (AZERTY)", () => {
  assert.equal(viewerShortcut(ev({ key: "_", shiftKey: true }), watching), "zoomOut");
  assert.equal(viewerShortcut(ev({ key: "+", shiftKey: true }), watching), "zoomIn");
  assert.equal(viewerShortcut(ev({ key: "0", shiftKey: true }), watching), "zoomFit");
  assert.equal(viewerShortcut(ev({ key: "1", shiftKey: true }), watching), "zoomActual");
  // A shifted US digit row produces other characters, which are not shortcuts.
  assert.equal(viewerShortcut(ev({ key: "!", shiftKey: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: ")", shiftKey: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: "-", ctrlKey: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: "_", altKey: true, shiftKey: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: "F", shiftKey: true }), watching), null, "Shift+F stays free");
});

const locale = (code: string) =>
  JSON.parse(readFileSync(new URL(`../i18n/locales/${code}.json`, import.meta.url), "utf8")) as {
    liveView: { toolbar: { keys: Record<string, unknown> } };
  };

test("every listed shortcut has a label in English and Turkish and none is listed twice", () => {
  const ids = VIEWER_SHORTCUTS.map((k) => k.id);
  assert.equal(new Set(ids).size, ids.length);
  assert.ok(ids.includes("fullscreen") && ids.includes("zoomActual"));
  for (const code of ["en", "tr"]) {
    const labels = locale(code).liveView.toolbar.keys;
    for (const id of ids) {
      const label = labels[id];
      assert.ok(typeof label === "string" && label.trim() !== "", `${code}: liveView.toolbar.keys.${id} is missing`);
    }
    assert.deepEqual(Object.keys(labels).sort(), [...ids].sort(), `${code}: a label has no listed shortcut`);
  }
});

test("no shortcut fires while typing into the remote screen or a text field", () => {
  assert.equal(viewerShortcut(ev({ key: "f" }), { interactive: true, stageFocused: false }), null);
  assert.equal(viewerShortcut(ev({ key: "f" }), { interactive: false, stageFocused: true }), null);
  assert.equal(viewerShortcut(ev({ key: "f", editable: true }), watching), null);
  assert.equal(viewerShortcut(ev({ key: "f", defaultPrevented: true }), watching), null);
});

test("toggleFit flips between fit and fill", () => {
  assert.equal(toggleFit("fit"), "fill");
  assert.equal(toggleFit("fill"), "fit");
});

test("the spoken status names reconnecting, stalled and connecting", () => {
  const base = { hasTarget: true, frameShown: true, disconnected: false, stalled: false };
  assert.equal(viewerAnnouncementKey(base), null);
  assert.equal(viewerAnnouncementKey({ ...base, stalled: true }), "stalled");
  assert.equal(viewerAnnouncementKey({ ...base, disconnected: true, stalled: true }), "reconnecting");
  assert.equal(viewerAnnouncementKey({ ...base, frameShown: false }), "connecting");
  assert.equal(viewerAnnouncementKey({ ...base, hasTarget: false, frameShown: false }), null);
});
