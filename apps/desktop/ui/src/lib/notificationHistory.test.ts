import assert from "node:assert/strict";
import test from "node:test";
import {
  addEntry, HISTORY_MAX, HISTORY_MAX_AGE_MS, markAllRead, markRead, parseHistory, unreadCount, type HistoryEntry
} from "./notificationHistory.ts";

const now = 1_800_000_000_000;
const e = (id: string, ts: number, patch: Partial<HistoryEntry> = {}): HistoryEntry => ({
  id, kind: "needsYou", title: "t", body: "b", botId: "b1", botName: "Bot", chatId: "c1", ts, shown: false, read: false, ...patch
});

test("keeps newest first and caps the count", () => {
  let list: HistoryEntry[] = [];
  for (let i = 0; i < HISTORY_MAX + 5; i++) list = addEntry(list, e(`n${i}`, now - 1000 + i), now);
  assert.equal(list.length, HISTORY_MAX);
  assert.equal(list[0].id, `n${HISTORY_MAX + 4}`);
  assert.equal(list.some((x) => x.id === "n0"), false);
});

test("drops entries past the retention age, also when parsing storage", () => {
  const old = e("old", now - HISTORY_MAX_AGE_MS - 1);
  assert.deepEqual(addEntry([old], e("new", now), now).map((x) => x.id), ["new"]);
  assert.deepEqual(parseHistory([old, e("ok", now), { kind: "bogus", ts: 1, id: "x" }, 5], now).map((x) => x.id), ["ok"]);
});

test("parsing keeps why a toast was held; older held entries mean quiet hours", () => {
  const parsed = parseHistory(
    [
      e("muted", now, { heldBy: "muted" }),
      e("old-held", now - 1),
      e("shown", now - 2, { shown: true, heldBy: "muted" }),
      { ...e("bogus", now - 3), heldBy: "later" }
    ],
    now
  );
  assert.deepEqual(
    parsed.map((x) => [x.id, x.heldBy]),
    [
      ["muted", "muted"],
      ["old-held", "quiet"],
      ["shown", undefined],
      ["bogus", "quiet"]
    ]
  );
});

test("mark one / all read and unread count; clear is an empty list", () => {
  let list = [e("a", now), e("b", now - 1)];
  assert.equal(unreadCount(list), 2);
  list = markRead(list, "a");
  assert.equal(unreadCount(list), 1);
  list = markAllRead(list);
  assert.equal(unreadCount(list), 0);
  assert.equal(markAllRead(list), list);
});
