import assert from "node:assert/strict";
import { test } from "node:test";
import { approvalKeyAction, type ApprovalKeyEvent } from "./approvalKeys.ts";

/**
 * A DOM element as far as the rules see it: which selectors it (or an ancestor) matches. `self` lists
 * the selectors the element itself matches (`closest` then returns the element, as in a browser).
 */
function el(tag: string, ancestry: string[], value?: string, self: string[] = []) {
  const node = {
    tagName: tag,
    value,
    closest: (sel: string): unknown => {
      const wanted = sel.split(",").map((x) => x.trim());
      if (self.some((x) => wanted.includes(x))) return node;
      return ancestry.some((x) => wanted.includes(x)) ? {} : null;
    }
  };
  return node;
}
const CARD = "[data-approval-card]";
/** The card root with the focus; and something inside the card with the focus. */
const card = () => el("DIV", [CARD], undefined, [CARD]);
const inCard = (tag: string, matches: string[], value?: string) => el(tag, [CARD, ...matches], value);

const ev = (key: string, target: unknown, extra: Partial<ApprovalKeyEvent> = {}): ApprovalKeyEvent => ({
  key,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  target,
  ...extra
});
const ready = { armed: true, canAllow: true };

test("Enter allows and Escape denies when the card itself has the focus", () => {
  assert.equal(approvalKeyAction(ev("Enter", card()), ready), "allow");
  assert.equal(approvalKeyAction(ev("Escape", card()), ready), "deny");
});

test("neither key decides a card from the page, the composer (empty or not) or any other control", () => {
  const body = el("BODY", []);
  const html = el("HTML", []);
  const empty = el("TEXTAREA", ["[data-composer-input]", "textarea"], "");
  const typed = el("TEXTAREA", ["[data-composer-input]", "textarea"], "hello");
  const search = el("INPUT", ["input"], "");
  const sidebar = el("BUTTON", ["button"]);
  for (const target of [body, html, empty, typed, search, sidebar, null, undefined]) {
    assert.equal(approvalKeyAction(ev("Enter", target), ready), null);
    assert.equal(approvalKeyAction(ev("Escape", target), ready), null);
  }
  // The dock around the card is not the card.
  assert.equal(approvalKeyAction(ev("Enter", el("SECTION", ["[data-approval-dock]"])), ready), null);
  assert.equal(approvalKeyAction(ev("Escape", el("SECTION", ["[data-approval-dock]"])), ready), null);
});

test("inside the card only the card itself answers Enter: a button, a link, a code block or a field never does", () => {
  const button = inCard("BUTTON", ["button"]);
  const code = inCard("PRE", []);
  const toggle = inCard("SUMMARY", ["summary"]);
  const reason = inCard("INPUT", ["input"], "");
  for (const target of [button, code, toggle, reason]) {
    assert.equal(approvalKeyAction(ev("Enter", target), ready), null);
  }
  // Escape still denies from a button or the text of the card, but not from its field.
  assert.equal(approvalKeyAction(ev("Escape", button), ready), "deny");
  assert.equal(approvalKeyAction(ev("Escape", code), ready), "deny");
  assert.equal(approvalKeyAction(ev("Escape", reason), ready), null);
});

test("a dialog, a modifier or an IME composition never decides a card", () => {
  const inModal = el("DIV", [CARD, "[role='dialog']"], undefined, [CARD]);
  assert.equal(approvalKeyAction(ev("Enter", inModal), ready), null);
  assert.equal(approvalKeyAction(ev("Enter", card(), { ctrlKey: true }), ready), null);
  assert.equal(approvalKeyAction(ev("Enter", card(), { shiftKey: true }), ready), null);
  assert.equal(approvalKeyAction(ev("Enter", card(), { isComposing: true }), ready), null);
  assert.equal(approvalKeyAction(ev("Enter", card(), { keyCode: 229 }), ready), null);
  assert.equal(approvalKeyAction(ev("Enter", card(), { defaultPrevented: true }), ready), null);
});

test("Enter never allows a card that must be read or that has only just appeared", () => {
  assert.equal(approvalKeyAction(ev("Enter", card()), { armed: false, canAllow: true }), null);
  assert.equal(approvalKeyAction(ev("Enter", card()), { armed: true, canAllow: false }), null);
  // Denying is always safe, so it needs neither.
  assert.equal(approvalKeyAction(ev("Escape", card()), { armed: false, canAllow: false }), "deny");
});
