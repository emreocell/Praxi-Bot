import assert from "node:assert/strict";
import { test } from "node:test";
import {
  loadChatOpen,
  saveChatOpen,
  toggleFit,
  viewerAnnouncementKey,
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

test("the chat strip is collapsed by default and remembers its state", () => {
  const store = new Map<string, string>();
  const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
  assert.equal(loadChatOpen(storage), false);
  saveChatOpen(true, storage);
  assert.equal(loadChatOpen(storage), true);
  saveChatOpen(false, storage);
  assert.equal(loadChatOpen(storage), false);
  const broken = {
    getItem: () => {
      throw new Error("blocked");
    },
    setItem: () => {
      throw new Error("blocked");
    }
  };
  assert.equal(loadChatOpen(broken), false);
  assert.doesNotThrow(() => saveChatOpen(true, broken));
});

test("the spoken status names reconnecting, stalled and connecting", () => {
  const base = { hasTarget: true, frameShown: true, disconnected: false, stalled: false };
  assert.equal(viewerAnnouncementKey(base), null);
  assert.equal(viewerAnnouncementKey({ ...base, stalled: true }), "stalled");
  assert.equal(viewerAnnouncementKey({ ...base, disconnected: true, stalled: true }), "reconnecting");
  assert.equal(viewerAnnouncementKey({ ...base, frameShown: false }), "connecting");
  assert.equal(viewerAnnouncementKey({ ...base, hasTarget: false, frameShown: false }), null);
});
