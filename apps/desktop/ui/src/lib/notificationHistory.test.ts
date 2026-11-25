import assert from "node:assert/strict";
import test from "node:test";
import {
  actionableCount,
  addEntry,
  collapseDuplicates,
  filterEntries,
  groupInbox,
  heldByQuietCount,
  HISTORY_MAX,
  HISTORY_MAX_AGE_MS,
  markAllRead,
  markIdsRead,
  markRead,
  NO_FILTER,
  parseHistory,
  resolveByCallId,
  resolveByIds,
  snooze,
  snoozeIds,
  unreadCount,
  unsnooze,
  unsnoozeIds,
  UPDATES_WINDOW_MS,
  type HistoryEntry
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

test("parsing keeps why a toast was held and optional metadata; older held entries mean quiet hours", () => {
  const parsed = parseHistory(
    [
      e("muted", now, { heldBy: "muted", runId: "r1", callId: "c1", retryable: true, resolved: "approved" }),
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
  assert.equal(parsed[0].runId, "r1");
  assert.equal(parsed[0].callId, "c1");
  assert.equal(parsed[0].resolved, "approved");
});

test("mark one / all read, resolve, and sidebar actionableCount", () => {
  let list = [
    e("needs-live", now, { kind: "needsYou", callId: "call-live" }),
    e("needs-gone", now - 1, { kind: "needsYou", callId: "call-gone" }),
    e("long-run", now - 2, { kind: "longRun" }),
    e("failed", now - 3, { kind: "failed" })
  ];
  const live = new Set(["call-live"]);
  assert.equal(unreadCount(list), 4);
  // Sidebar badge counts only actionable items that are still live & unread.
  assert.equal(actionableCount(list, now, live), 1);

  list = resolveByCallId(list, "call-live", "approved");
  assert.equal(actionableCount(list, now, live), 0);
  assert.equal(unreadCount(list), 3);

  list = markIdsRead(list, ["long-run"]);
  assert.equal(unreadCount(list), 2);
  list = markRead(list, "failed");
  assert.equal(unreadCount(list), 1);
  list = markAllRead(list);
  assert.equal(unreadCount(list), 0);
  assert.equal(markAllRead(list), list);

  const resolved = resolveByIds([e("x", now)], ["x"], "denied");
  assert.equal(resolved[0].resolved, "denied");
  assert.equal(resolved[0].read, true);
});

test("snoozed entries leave the badge and the inbox until the time passes, and survive reload", () => {
  const until = now + 1000;
  const list = snoozeIds([e("a", now), e("b", now - 1)], ["a"], until);
  assert.equal(unreadCount(list, now), 1);
  assert.equal(actionableCount(list, now), 1);
  assert.equal(unreadCount(list, until + 1), 2);
  assert.deepEqual(filterEntries(list, NO_FILTER, now).map((x) => x.id), ["b"]);
  assert.deepEqual(filterEntries(list, { ...NO_FILTER, snoozed: true }, now).map((x) => x.id), ["a"]);
  assert.deepEqual(filterEntries(list, NO_FILTER, until + 1).map((x) => x.id), ["a", "b"]);
  assert.equal(parseHistory(JSON.parse(JSON.stringify(list)), now)[0].snoozedUntil, until);
  assert.equal(unsnoozeIds(list, ["a"])[0].snoozedUntil, undefined);
  assert.equal(unsnooze(snooze([e("a", now)], "a", until), "a")[0].snoozedUntil, undefined);
});

test("filter chips filter by all, needsYou, runs, errors, and unread", () => {
  const list = [
    e("a", now, { kind: "failed", read: true }),
    e("b", now - 1, { kind: "failed" }),
    e("c", now - 2, { kind: "needsYou", heldBy: "quiet" }),
    e("d", now - 3, { kind: "longRun", heldBy: "muted" }),
    e("e", now - 4, { kind: "scheduleDone" }),
    e("f", now - 5, { kind: "digest" })
  ];
  assert.deepEqual(filterEntries(list, { ...NO_FILTER, category: "errors" }, now).map((x) => x.id), ["a", "b"]);
  assert.deepEqual(filterEntries(list, { ...NO_FILTER, category: "errors", unreadOnly: true }, now).map((x) => x.id), ["b"]);
  assert.deepEqual(filterEntries(list, { ...NO_FILTER, category: "runs" }, now).map((x) => x.id), ["d", "e", "f"]);
  assert.deepEqual(filterEntries(list, { ...NO_FILTER, category: "needsYou" }, now).map((x) => x.id), ["c"]);
  assert.equal(heldByQuietCount(list, now), 1);
});

test("collapses duplicate notifications per bot and prefers live pending callId", () => {
  const list = [
    e("n1", now - 40_000, { title: "Praxi needs your OK to open webioo.com.tr", callId: "old-1" }),
    e("n2", now - 30_000, { title: "Praxi needs your OK to open webioo.com.tr", callId: "live-2" }),
    e("n3", now - 20_000, { title: "Praxi needs your OK to open webioo.com.tr", callId: "old-3", read: true }),
    e("n4", now - 10_000, { title: "Praxi needs your OK to open webioo.com.tr" }),
    e("other", now - 5_000, { botId: "b2", title: "Nova needs your OK to open example.org" })
  ];
  const collapsed = collapseDuplicates(list, new Set(["live-2"]));
  assert.equal(collapsed.length, 2);
  const atlas = collapsed.find((g) => g.primary.botId === "b1");
  assert.ok(atlas);
  assert.equal(atlas.count, 4);
  assert.equal(atlas.activeCallId, "live-2");
  assert.equal(atlas.unread, true);
  assert.equal(atlas.ids.length, 4);
});

test("groups collapsed items into Needs you, Updates, and Earlier", () => {
  const list = [
    e("need-1", now - 1000, { kind: "needsYou", callId: "c-live" }),
    e("update-1", now - 2000, { kind: "longRun", read: false }),
    e("update-recent-read", now - 3000, { kind: "scheduleDone", read: true }),
    e("earlier-1", now - UPDATES_WINDOW_MS - 5000, { kind: "digest", read: true })
  ];
  const grouped = groupInbox(list, now, new Set(["c-live"]));
  assert.deepEqual(grouped.needsYou.map((g) => g.primary.id), ["need-1"]);
  assert.deepEqual(grouped.updates.map((g) => g.primary.id), ["update-1", "update-recent-read"]);
  assert.deepEqual(grouped.earlier.map((g) => g.primary.id), ["earlier-1"]);
});

test("sidebar badge counts a repeated approval once, like the inbox group", () => {
  const list = [1, 2, 3, 4].map((n) => e(`ask-${n}`, now - n, { callId: "call-live" }));
  assert.equal(actionableCount(list, now, new Set(["call-live"])), 1);
});
