import assert from "node:assert/strict";
import { test } from "node:test";
import type { Mission, MissionItem } from "../api/types.ts";
import {
  buildAnswers,
  currentItem,
  itemLabel,
  missionForChat,
  missionNote,
  isUnanswered,
  missionChip,
  missionCounts,
  missionHeadline,
  missionsToShow,
  progressPercent,
  shouldPoll
} from "./missions.ts";

function mission(id: string, state: string, extra: Partial<Mission> = {}): Mission {
  return {
    id,
    bot_id: "bot",
    chat_id: "chat",
    title: id,
    goal: "goal",
    state,
    state_reason: null,
    progress: { total: 15, done: 7, blocked: 2, todo: 6, doing: 0, failed: 0, skipped: 0, left: 6 },
    target_done: null,
    sessions: 3,
    model_steps: 120,
    cost_usd: 0.4,
    session_running: false,
    budgets: {
      max_sessions: 40,
      max_model_steps: 2000,
      max_minutes: 600,
      max_cost_usd: null,
      max_stalled_sessions: 3,
      max_failed_sessions: 3
    },
    started_at_ms: 1_000,
    updated_at_ms: 1_000,
    finished_at_ms: null,
    ...extra
  } as Mission;
}

test("progress percentage rounds, clamps and survives an empty ledger", () => {
  assert.equal(progressPercent({ done: 7, total: 15 }), 47);
  assert.equal(progressPercent({ done: 0, total: 0 }), 0);
  assert.equal(progressPercent({ done: 3, total: 3 }), 100);
  assert.equal(progressPercent({ done: 9, total: 3 }), 100);
});

test("the three numbers are the daemon's, without overlap", () => {
  assert.deepEqual(missionCounts({ done: 7, blocked: 2, left: 6 }), { done: 7, blocked: 2, left: 6 });
  assert.deepEqual(missionCounts({ done: 0, blocked: 0, left: 0 }), { done: 0, blocked: 0, left: 0 });
});

test("the block shows every active mission and only the newest finished one", () => {
  const list = [
    mission("old-done", "done", { finished_at_ms: 2_000, updated_at_ms: 2_000 }),
    mission("run", "running", { updated_at_ms: 5_000 }),
    mission("new-failed", "failed", { finished_at_ms: 9_000, updated_at_ms: 9_000 }),
    mission("blk", "blocked", { updated_at_ms: 6_000 }),
    mission("gone", "cancelled", { finished_at_ms: 3_000, updated_at_ms: 3_000 })
  ];
  assert.deepEqual(
    missionsToShow(list).map((m) => m.id),
    ["blk", "run", "new-failed"]
  );
  assert.deepEqual(missionsToShow([]), []);
  assert.deepEqual(
    missionsToShow([mission("a", "done", { finished_at_ms: 1, updated_at_ms: 1 })]).map((m) => m.id),
    ["a"]
  );
});

test("polling needs a visible tab and a mission that can still change", () => {
  const active = [mission("a", "paused")];
  const ended = [mission("b", "done"), mission("c", "cancelled")];
  assert.equal(shouldPoll(active, true), true);
  assert.equal(shouldPoll(active, false), false);
  assert.equal(shouldPoll(ended, true), false);
  assert.equal(shouldPoll([], true), false);
});

test("blocked items: touched ones are sent, untouched ones are left out", () => {
  const blocked = [{ key: "a.com" }, { key: "b.com" }, { key: "c.com" }];
  const r = buildAnswers(blocked, {
    "a.com": { text: "  123456 ", skip: false },
    "c.com": { text: "typed but skipped", skip: true }
  });
  assert.deepEqual(r.answers, [{ key: "a.com", answer: "123456" }, { key: "c.com", skip: true }]);
  assert.deepEqual(r.unanswered, []);
  assert.equal(r.canSend, true);
});

test("a touched item with no text and no skip is unanswered and holds Send back", () => {
  assert.equal(isUnanswered({ text: "   ", skip: false }), true);
  assert.equal(isUnanswered({ text: "", skip: true }), false);
  const r = buildAnswers([{ key: "a.com" }, { key: "b.com" }], {
    "a.com": { text: "yes", skip: false },
    "b.com": { text: "", skip: false }
  });
  assert.deepEqual(r.unanswered, ["b.com"]);
  assert.equal(r.canSend, false);
});

test("nothing touched, or only items that are no longer blocked, cannot be sent", () => {
  assert.equal(buildAnswers([{ key: "a.com" }], {}).canSend, false);
  const r = buildAnswers([{ key: "a.com" }], { "gone.com": { text: "x", skip: false } });
  assert.deepEqual(r.answers, []);
  assert.equal(r.canSend, false);
});

test("the chip and the headline follow the state", () => {
  assert.equal(missionChip("blocked"), "needsYou");
  assert.equal(missionChip("cancelled"), "stopped");
  assert.equal(missionChip("running"), "running");
  assert.equal(missionHeadline(mission("a", "running")).key, "missions.headline.running");
  assert.equal(
    missionHeadline(mission("a", "running", { progress: { ...mission("x", "running").progress, total: 0 } })).key,
    "missions.headline.planning"
  );
  assert.equal(missionHeadline(mission("a", "blocked")).key, "missions.headline.needsYou");
  assert.equal(
    missionHeadline(mission("a", "blocked", { progress: { ...mission("x", "running").progress, blocked: 0 } })).key,
    "missions.headline.stuck"
  );
  assert.deepEqual(missionHeadline(mission("a", "done")).values, { done: 7, blocked: 2, left: 6, total: 15 });
  assert.equal(missionHeadline(mission("a", "cancelled")).key, "missions.headline.stopped");
});

function item(key: string, status: string, at: number, title = `Register on ${key}`): MissionItem {
  return { key, kind: "site", title, status, attempts: 1, result: null, last_error: null, blocked_reason: null, needs_live_owner: false, owner_answer: null, updated_at_ms: at } as MissionItem;
}

test("the current item is the newest one being worked on", () => {
  assert.equal(currentItem([item("a.io", "done", 9), item("b.io", "doing", 3), item("c.io", "doing", 5), item("d.io", "todo", 8)])?.key, "c.io");
  assert.equal(currentItem([item("a.io", "done", 9)]), null);
});

test("an item reads as one phrase without repeating its key", () => {
  assert.equal(itemLabel({ key: "hotel.io", title: "Register on hotel.io" }), "Register on hotel.io");
  assert.equal(itemLabel({ key: "hotel.io", title: "Sign up" }), "hotel.io · Sign up");
  assert.equal(itemLabel({ key: "hotel.io", title: "" }), "hotel.io");
});

test("a chat shows its own mission: the active one first, else the newest that ended there", () => {
  const all = [
    mission("old", "done", { chat_id: "c1", finished_at_ms: 10, updated_at_ms: 10 }),
    mission("live", "running", { chat_id: "c1", updated_at_ms: 5 }),
    mission("other", "running", { chat_id: "c2", updated_at_ms: 50 })
  ];
  assert.equal(missionForChat(all, "c1")?.id, "live");
  assert.equal(missionForChat(all.slice(0, 1), "c1")?.id, "old");
  assert.equal(missionForChat(all, "c3"), null);
  assert.equal(missionForChat(all, undefined), null);
});

test("the note says the owner can leave while it runs, answer while it waits, nothing once it ended", () => {
  assert.equal(missionNote({ state: "running" }), "leave");
  assert.equal(missionNote({ state: "blocked" }), "answer");
  assert.equal(missionNote({ state: "paused" }), "paused");
  assert.equal(missionNote({ state: "done" }), null);
  assert.equal(missionNote({ state: "cancelled" }), null);
});
