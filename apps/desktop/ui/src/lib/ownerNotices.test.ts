import assert from "node:assert/strict";
import { test } from "node:test";
import type { OwnerNotice } from "../api/types.ts";
import type { HistoryEntry } from "./notificationHistory.ts";
import { freshNotices, latestSeq, noticeEntry, noticeEntryId } from "./ownerNotices.ts";

const t = (k: string, v?: Record<string, unknown>) => `${k}${v ? JSON.stringify(v) : ""}`;

const notice = (over: Partial<OwnerNotice> = {}): OwnerNotice => ({
  seq: 1,
  id: "n1",
  at_ms: 1_000,
  kind: "blocked",
  source: "mission",
  title: "Backlinks",
  bot_name: "Scout",
  bot_id: "bot-1",
  chat_id: "chat-1",
  mission_id: "m1",
  progress: { total: 10, done: 6, blocked: 2, failed: 1, skipped: 1 },
  done: [],
  failed: [],
  questions: [{ key: "a", title: "site a", text: "Which name?" }],
  app_only: ["Pay $9"],
  can_retry: false,
  late: false,
  ...over
});

const entry = (over: Partial<HistoryEntry>): HistoryEntry => ({
  id: "x",
  kind: "scheduleDone",
  title: "",
  body: "",
  botId: "",
  botName: "",
  chatId: "",
  ts: 1,
  shown: true,
  read: false,
  ...over
});

test("a blocked mission becomes a needs-you entry with the counts and what waits", () => {
  const e = noticeEntry(notice(), t, { private: false });
  assert.equal(e.id, noticeEntryId(notice()));
  assert.equal(e.kind, "needsYou");
  assert.equal(e.botId, "bot-1");
  assert.equal(e.chatId, "chat-1");
  assert.equal(e.ts, 1_000);
  assert.equal(e.shown, true);
  assert.equal(e.read, false);
  assert.match(e.title, /ownerNotice\.title\.blocked/);
  assert.match(e.body, /ownerNotice\.counts.*"done":6,"total":10,"blocked":2,"failed":1/);
  assert.match(e.body, /ownerNotice\.questions.*"count":1/);
  assert.match(e.body, /ownerNotice\.inApp.*"count":1/);
});

test("finished, failed and progress map onto the existing history kinds", () => {
  assert.equal(noticeEntry(notice({ kind: "finished", questions: [], app_only: [] }), t, { private: false }).kind, "scheduleDone");
  assert.equal(noticeEntry(notice({ kind: "progress" }), t, { private: false }).kind, "scheduleDone");
  const failed = noticeEntry(notice({ kind: "failed", reason: "provider unreachable" }), t, { private: false });
  assert.equal(failed.kind, "failed");
  assert.match(failed.body, /provider unreachable/);
});

test("a finished run shows what it said, a progress milestone shows no counts line", () => {
  const run = notice({
    kind: "finished",
    source: "run",
    run_id: "r1",
    progress: undefined,
    questions: [],
    app_only: [],
    done: [{ key: "r1", title: "Report", text: "Posted 3 leads" }]
  });
  const e = noticeEntry(run, t, { private: false });
  assert.equal(e.runId, "r1");
  assert.match(e.body, /Posted 3 leads/);
  const p = noticeEntry(notice({ kind: "progress", questions: [], app_only: [] }), t, { private: false });
  assert.doesNotMatch(p.body, /ownerNotice\.counts/);
});

test("private mode keeps the title, the bot and every word of the digest out of the entry", () => {
  const e = noticeEntry(notice({ reason: "SECRET REASON" }), t, { private: true });
  assert.equal(e.title, "ownerNotice.private.blocked");
  assert.equal(e.body, "");
  assert.equal(e.botName, "");
  assert.doesNotMatch(JSON.stringify(e), /Backlinks|Scout|SECRET/);
});

test("a notice already in the history is not added again and keeps its read state", () => {
  const seen = entry({ id: noticeEntryId(notice()), read: true });
  assert.deepEqual(freshNotices([notice()], [seen]), []);
  assert.equal(freshNotices([notice(), notice({ id: "n2" })], [seen]).length, 1);
});

test("a background run the app already announced from its event stream is not announced twice", () => {
  const run = notice({ source: "run", run_id: "r1", kind: "finished" });
  assert.deepEqual(freshNotices([run], [entry({ id: "run-r1-done", runId: "r1" })]), []);
  assert.equal(freshNotices([run], [entry({ id: "other", runId: "r2" })]).length, 1);
  // A mission has no live entry of its own: its digest is always added.
  const mission = notice({ source: "mission", run_id: "r1" });
  assert.equal(freshNotices([mission], [entry({ id: "z", runId: "r1" })]).length, 1);
});

test("the cursor is the newest seq seen", () => {
  assert.equal(latestSeq([]), 0);
  assert.equal(latestSeq([notice({ seq: 3 }), notice({ seq: 9, id: "b" })], 4), 9);
  assert.equal(latestSeq([notice({ seq: 3 })], 5), 5);
});
