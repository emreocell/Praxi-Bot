import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  composerPlaceholderKey,
  shouldFocusOnBoxClick,
  shouldTypeToFocus
} from "./composerFocus.ts";

/** A fake element: `closest` answers true when its tags/roles match one of the selector parts. */
function el(...tags: string[]) {
  return {
    closest(sel: string) {
      return sel.split(",").some((part) => tags.some((t) => part.trim().startsWith(t) || part.includes(`'${t}'`))) ? this : null;
    }
  };
}
const key = (k: string, target: unknown, extra: object = {}) => ({
  key: k,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  target,
  ...extra
});
const free = { composerReady: true, dialogOpen: false, selectionActive: false };

test("the busy hint is a placeholder key, never composer text", () => {
  assert.equal(composerPlaceholderKey({ awaitingApproval: false, runActive: true }), "chat.placeholderBusy");
  assert.equal(composerPlaceholderKey({ awaitingApproval: true, runActive: true }), "chat.placeholderWaiting");
  assert.equal(composerPlaceholderKey({ awaitingApproval: false, runActive: false }), "chat.placeholder");
  const src = readFileSync(new URL("../views/ChatView.tsx", import.meta.url), "utf8");
  assert.match(src, /placeholder=\{t\(composerPlaceholderKey\(/);
  assert.doesNotMatch(src, /setText\(t\("chat\.placeholder/);
  for (const lang of ["en", "tr", "de"]) {
    const j = JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8"));
    assert.ok(j.chat.placeholderBusy.length > 0 && !j.chat.placeholderBusy.endsWith("…"), lang);
  }
});

test("clicking the empty composer surface focuses the field, clicking a control does not", () => {
  const box = { contains: () => true };
  assert.equal(shouldFocusOnBoxClick(el("div"), box), true);
  assert.equal(shouldFocusOnBoxClick(el("button"), box), false);
  assert.equal(shouldFocusOnBoxClick(el("textarea"), box), false);
  assert.equal(shouldFocusOnBoxClick(el("div"), { contains: () => false }), false);
  assert.equal(shouldFocusOnBoxClick(null, box), false);
});

test("typing a character anywhere in the chat focuses the composer", () => {
  assert.equal(shouldTypeToFocus(key("a", el("div")), free), true);
  assert.equal(shouldTypeToFocus(key(" ", el("button")), free), false);
});

test("type-to-focus leaves shortcuts, other inputs, dialogs, selections and the IME alone", () => {
  assert.equal(shouldTypeToFocus(key("c", el("div"), { ctrlKey: true }), free), false);
  assert.equal(shouldTypeToFocus(key("k", el("div"), { metaKey: true }), free), false);
  assert.equal(shouldTypeToFocus(key("Enter", el("div")), free), false);
  assert.equal(shouldTypeToFocus(key("Escape", el("div")), free), false);
  assert.equal(shouldTypeToFocus(key("a", el("input")), free), false);
  assert.equal(shouldTypeToFocus(key("a", el("div")), { ...free, dialogOpen: true }), false);
  assert.equal(shouldTypeToFocus(key("a", el("div")), { ...free, selectionActive: true }), false);
  assert.equal(shouldTypeToFocus(key("a", el("div")), { ...free, composerReady: false }), false);
  assert.equal(shouldTypeToFocus(key("a", el("div"), { isComposing: true }), free), false);
  assert.equal(shouldTypeToFocus(key("Process", el("div"), { keyCode: 229 }), free), false);
});

test("action icons carry a tooltip and an accessible name, and the actions share one wrapping row", () => {
  const bar = readFileSync(new URL("../components/RunActionBar.tsx", import.meta.url), "utf8");
  // The label shows right above the icon on hover / focus (one tooltip, no OS `title` on top of it).
  assert.match(bar, /<span className=\{tipStyles\.tip\} aria-hidden="true">\{label\}<\/span>/);
  assert.doesNotMatch(bar, /title=\{label\}/);
  assert.match(bar, /aria-label=\{label\}/);
  assert.match(bar, /role="group"/);
  const css = readFileSync(new URL("../components/RunActionBar.module.css", import.meta.url), "utf8");
  assert.match(css, /\.bar \{[^}]*flex-wrap: wrap/);
  const chat = readFileSync(new URL("../views/ChatView.tsx", import.meta.url), "utf8");
  for (const id of ["run-timeline", "remember-run"]) assert.ok(chat.includes(`testId="${id}"`), id);
});
