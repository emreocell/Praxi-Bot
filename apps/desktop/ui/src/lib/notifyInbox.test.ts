import assert from "node:assert/strict";
import test from "node:test";
import { collapseDuplicates, type HistoryEntry } from "./notificationHistory.ts";
import { inboxByBot, notifyType, primaryAction, BOT_PREVIEW_MAX } from "./notifyInbox.ts";

const now = 1_800_000_000_000;
const e = (id: string, ts: number, patch: Partial<HistoryEntry> = {}): HistoryEntry => ({
  id, kind: "longRun", title: id, body: "", botId: "b1", botName: "Atlas", chatId: "c1", ts, shown: true, read: false, ...patch
});
const one = (entry: HistoryEntry, live?: ReadonlySet<string>) => collapseDuplicates([entry], live)[0];

test("notifyType: an open question needs you, a failure failed, everything else finished", () => {
  assert.equal(notifyType(one(e("q", now, { kind: "needsYou", callId: "k" }), new Set(["k"])), new Set(["k"])), "needsYou");
  // The question was answered elsewhere (no longer live): nothing waits, it reads as done.
  assert.equal(notifyType(one(e("q", now, { kind: "needsYou", callId: "k" })), new Set()), "finished");
  assert.equal(notifyType(one(e("q", now, { kind: "needsYou", resolved: "approved" }))), "finished");
  assert.equal(notifyType(one(e("f", now, { kind: "failed" }))), "failed");
  assert.equal(notifyType(one(e("s", now, { kind: "scheduleDone" }))), "finished");
  assert.equal(notifyType(one(e("d", now, { kind: "digest", botId: "" }))), "finished");
});

test("primaryAction: one button per row — reply, try again or open", () => {
  const failed = one(e("f", now, { kind: "failed" }));
  assert.equal(primaryAction(failed, "failed", false), "retry");
  assert.equal(primaryAction(failed, "failed", true), "open", "private mode never re-sends from the inbox");
  assert.equal(primaryAction(one(e("f", now, { kind: "failed", chatId: "" })), "failed", false), "open");
  assert.equal(primaryAction(one(e("f", now, { kind: "failed", resolved: "retried" })), "failed", false), "open");
  assert.equal(primaryAction(one(e("q", now, { kind: "needsYou" })), "needsYou", false), "reply");
  assert.equal(primaryAction(one(e("x", now)), "finished", false), "open");
});

test("inboxByBot: bots that need you first, then unread failures, then newest; rows by type inside a bot", () => {
  const live = new Set(["k"]);
  const entries = [
    e("atlas-done", now - 1_000, { botId: "b1" }),
    e("atlas-failed", now - 50_000, { botId: "b1", kind: "failed" }),
    e("nova-done", now - 500, { botId: "b2", botName: "Nova" }),
    e("milo-asks", now - 90_000, { botId: "b3", botName: "Milo", kind: "needsYou", callId: "k" }),
    e("ember-failed-read", now - 100, { botId: "b4", botName: "Ember", kind: "failed", read: true })
  ];
  const groups = inboxByBot(collapseDuplicates(entries, live), live);
  assert.deepEqual(groups.map((g) => g.botId), ["b3", "b1", "b4", "b2"]);
  const atlas = groups[1];
  assert.deepEqual(atlas.rows.map((r) => r.item.primary.id), ["atlas-failed", "atlas-done"]);
  assert.deepEqual(atlas.counts, { needsYou: 0, failed: 1, finished: 1 });
  assert.equal(atlas.unread, 2);
  assert.equal(atlas.botName, "Atlas");
});

test("inboxByBot: an old failure already read sorts by time, not above newer unread news", () => {
  const groups = inboxByBot(collapseDuplicates([
    e("old-failed", now - 90_000_000, { kind: "failed", read: true }),
    e("new-done", now - 1_000),
    e("new-failed", now - 2_000, { kind: "failed" })
  ]));
  assert.deepEqual(groups[0].rows.map((r) => r.item.primary.id), ["new-failed", "new-done", "old-failed"]);
});

test("inboxByBot: the system bot (no id) is its own group and the preview size is small", () => {
  const groups = inboxByBot(collapseDuplicates([e("d", now, { kind: "digest", botId: "", botName: "" })]));
  assert.equal(groups.length, 1);
  assert.equal(groups[0].botId, "");
  assert.ok(BOT_PREVIEW_MAX >= 3 && BOT_PREVIEW_MAX <= 5);
});
