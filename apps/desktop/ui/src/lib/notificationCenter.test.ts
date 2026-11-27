import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

test("notification center: grouped by bot, one primary action per row, no inline Allow/Deny", () => {
  const view = read("../views/NotificationHistoryView.tsx");
  assert.match(view, /inboxByBot\(collapseDuplicates\(visible, liveCallIds\), liveCallIds\)/);
  assert.equal(view.match(/data-testid=\{`notify-action-\$\{action\}`\}/g)?.length, 1);
  assert.doesNotMatch(view, /notify-inline-(allow|deny|retry)/);
  assert.match(view, /t\(`notifyHistory\.action\.\$\{action\}`\)/);
});

test("notification center: Mark all read is the stronger header button while something is unread; no header buttons when empty", () => {
  const view = read("../views/NotificationHistoryView.tsx");
  assert.match(view, /variant=\{unread > 0 \? "secondary" : "ghost"\}/);
  assert.match(view, /\{list\.length > 0 \? \(\s*<div className=\{styles\.actions\}>/);
});
