import assert from "node:assert/strict";
import { test } from "node:test";
import { FRESH, type HistoryKey, type HistoryNav, afterEdit, historyKey, userMessageTexts } from "./composerHistory.ts";

const key = (k: string, over: Partial<HistoryKey> = {}): HistoryKey => ({
  key: k,
  shift: false,
  ctrl: false,
  meta: false,
  alt: false,
  composing: false,
  menuOpen: false,
  text: "",
  selStart: 0,
  selEnd: 0,
  ...over
});

const history = ["first", "second\nline two", "third"];

test("only the chat's own non-empty messages count, oldest first, repeats collapsed", () => {
  assert.deepEqual(
    userMessageTexts([
      { role: "user", content: " first " },
      { role: "assistant", content: "hi" },
      { role: "user", content: "first" },
      { role: "user", content: "" },
      { role: "user", content: "second" }
    ]),
    ["first", "second"]
  );
});

test("Up in an empty composer recalls the previous message, most recent first, and keeps walking back", () => {
  let nav: HistoryNav = FRESH;
  let text = "";
  const seen: string[] = [];
  for (let i = 0; i < 4; i++) {
    const r = historyKey(nav, history, key("ArrowUp", { text, selStart: 0, selEnd: 0 }));
    if (!r) {
      break;
    }
    nav = r.nav;
    text = r.text;
    seen.push(text);
  }
  assert.deepEqual(seen, ["third", "second\nline two", "first"], "stops at the oldest");
});

test("the draft is kept: Down walks back towards it and Esc restores it", () => {
  const draft = "half-typed";
  const up = historyKey(FRESH, history, key("ArrowUp", { text: draft, selStart: 0, selEnd: 0 }))!;
  assert.equal(up.text, "third");
  assert.equal(up.nav.draft, draft);
  const up2 = historyKey(up.nav, history, key("ArrowUp", { text: up.text, selStart: 0, selEnd: 0 }))!;
  const down = historyKey(up2.nav, history, key("ArrowDown", { text: up2.text, selStart: up2.text.length, selEnd: up2.text.length }))!;
  assert.equal(down.text, "third");
  const toDraft = historyKey(down.nav, history, key("ArrowDown", { text: down.text, selStart: 5, selEnd: 5 }))!;
  assert.equal(toDraft.text, draft);
  assert.equal(toDraft.nav.index, null);
  // Esc from the middle of the history
  const esc = historyKey(up2.nav, history, key("Escape", { text: up2.text }))!;
  assert.equal(esc.text, draft);
  assert.equal(esc.nav.index, null);
  // Esc with nothing to restore is not ours
  assert.equal(historyKey(FRESH, history, key("Escape")), null);
});

test("multi-line messages: arrows move inside the box until the caret is on the first / last line", () => {
  const at = (nav: HistoryNav, text: string, pos: number) => (k: string) =>
    historyKey(nav, history, key(k, { text, selStart: pos, selEnd: pos }));
  const browsing: HistoryNav = { index: 1, draft: "d" };
  const text = history[1]; // "second\nline two"
  // caret on the second line: Up is the browser's (moves the caret up), not a recall
  assert.equal(at(browsing, text, text.length)("ArrowUp"), null);
  // caret on the first line: Up goes to the older message
  assert.equal(at(browsing, text, 2)("ArrowUp")?.text, "first");
  // caret on the first line: Down is the browser's; on the last line it recalls the newer one
  assert.equal(at(browsing, text, 2)("ArrowDown"), null);
  assert.equal(at(browsing, text, text.length)("ArrowDown")?.text, "third");
  // a non-empty draft with the caret below its first line is not touched either
  assert.equal(historyKey(FRESH, history, key("ArrowUp", { text: "a\nb", selStart: 3, selEnd: 3 })), null);
});

test("the slash / mention popup keeps its arrows; IME, modifiers and selections are never a recall", () => {
  assert.equal(historyKey(FRESH, history, key("ArrowUp", { menuOpen: true })), null);
  assert.equal(historyKey(FRESH, history, key("ArrowUp", { composing: true })), null);
  for (const mod of ["shift", "ctrl", "meta", "alt"] as const) {
    assert.equal(historyKey(FRESH, history, key("ArrowUp", { [mod]: true })), null, mod);
  }
  assert.equal(historyKey(FRESH, history, key("ArrowUp", { text: "abc", selStart: 0, selEnd: 3 })), null);
  assert.equal(historyKey(FRESH, [], key("ArrowUp")), null, "nothing to recall");
});

test("typing in a recalled message makes it the new draft", () => {
  assert.deepEqual(afterEdit({ index: 2, draft: "old" }, "edited"), { index: null, draft: "edited" });
  assert.deepEqual(afterEdit(FRESH, "x"), FRESH);
});
