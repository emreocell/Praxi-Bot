import assert from "node:assert/strict";
import { test } from "node:test";
import type { Mission, MissionItem, MissionWait } from "../api/types.ts";
import {
  buildAnswers,
  currentItem,
  isActiveMission,
  itemLabel,
  missionForChat,
  missionNote,
  isUnanswered,
  missionChip,
  missionCounts,
  missionHeadline,
  missionsStartedIn,
  missionsToShow,
  progressPercent,
  progressShort,
  shouldPoll,
  waitingItems,
  waitLine,
  whenText
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
    progress: { total: 15, done: 7, blocked: 2, waiting: 0, todo: 6, doing: 0, failed: 0, skipped: 0, left: 6 },
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
  assert.deepEqual(missionCounts({ done: 7, blocked: 2, waiting: 1, left: 6 }), { done: 7, blocked: 2, waiting: 1, left: 6 });
  assert.deepEqual(missionCounts({ done: 0, blocked: 0, waiting: 0, left: 0 }), { done: 0, blocked: 0, waiting: 0, left: 0 });
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
  assert.deepEqual(missionHeadline(mission("a", "done")).values, { done: 7, blocked: 2, waiting: 0, left: 6, total: 15 });
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

test("a mission that waits for a wake is active, polled and has its own chip and headline", () => {
  const waiting = mission("w", "waiting", { progress: { ...mission("x", "running").progress, waiting: 3, left: 0 } });
  assert.equal(isActiveMission(waiting), true);
  assert.equal(shouldPoll([waiting], true), true);
  assert.equal(missionChip("waiting"), "waiting");
  const h = missionHeadline(waiting);
  assert.equal(h.key, "missions.headline.waiting");
  assert.equal(h.values.waiting, 3);
  assert.deepEqual(
    missionsToShow([waiting, mission("d", "done", { finished_at_ms: 1, updated_at_ms: 1 })]).map((m) => m.id),
    ["w", "d"]
  );
});

function wait(extra: Partial<MissionWait>): MissionWait {
  return {
    kind: "mail",
    from_domain: null,
    subject_contains: null,
    keyword: null,
    webhook_name: null,
    due_at_ms: Date.UTC(2026, 10, 27, 14, 0),
    reason: "verification mail",
    ...extra
  };
}

test("a wait reads as what it waits for, with the time it ends", () => {
  const now = Date.UTC(2026, 10, 27, 9, 0);
  const at = (w: MissionWait) => waitLine(w, now, "en");
  assert.equal(at(wait({ kind: "after" })).key, "missions.waiting.after");
  const from = at(wait({ from_domain: "example.com", subject_contains: "verify" }));
  assert.equal(from.key, "missions.waiting.mailFrom");
  assert.equal(from.values.from, "example.com");
  assert.equal(at(wait({ subject_contains: "verify" })).key, "missions.waiting.mailSubject");
  const text = at(wait({ keyword: "confirm" }));
  assert.equal(text.key, "missions.waiting.mailText");
  assert.equal(text.values.text, "confirm");
  const hook = at(wait({ kind: "webhook", webhook_name: "Orders" }));
  assert.equal(hook.key, "missions.waiting.webhook");
  assert.equal(hook.values.name, "Orders");
  assert.ok(hook.values.when.length > 0);
});

test("a moment today shows the time only, another day shows the date too", () => {
  const now = Date.UTC(2026, 10, 27, 9, 0);
  const sameDay = whenText(now + 60_000, now, "en");
  const later = whenText(now + 3 * 86_400_000, now, "en");
  assert.ok(later.length > sameDay.length, `${later} vs ${sameDay}`);
});

test("only waiting items are listed as waiting", () => {
  const it = (key: string, status: string) => ({ key, status }) as MissionItem;
  assert.deepEqual(
    waitingItems([it("a", "todo"), it("b", "waiting"), it("c", "blocked")]).map((i) => i.key),
    ["b"]
  );
});

test("the cards of a chat: missions /mission started there, and missions the bot started from its messages", () => {
  const linked = mission("linked", "running", { chat_id: "mission-chat-1", started_at_ms: 30 });
  const asked = mission("asked", "done", { chat_id: "mission-chat-2", goal: "Sign up on 20 sites", started_at_ms: 20 });
  const elsewhere = mission("elsewhere", "running", { chat_id: "mission-chat-3", goal: "Something else", started_at_ms: 10 });
  const own = mission("own", "running", { chat_id: "here", goal: "Sign up on 20 sites", started_at_ms: 40 });
  const before = mission("before", "running", { chat_id: "mission-chat-4", goal: "Sign up on 20 sites", started_at_ms: 1 });
  const messages = [{ text: "  Sign up on 20 sites ", ts: 5 }];
  const shown = missionsStartedIn([linked, asked, elsewhere, own, before], "here", messages, ["linked"]);
  // Oldest first; the chat's own mission (it has the header) and one that started before the message are not cards.
  assert.deepEqual(shown.map((m) => m.id), ["asked", "linked"]);
  assert.deepEqual(missionsStartedIn([linked], undefined, messages, ["linked"]), []);
  assert.deepEqual(missionsStartedIn([mission("empty", "running", { goal: " ", chat_id: "x" })], "here", [{ text: "", ts: 0 }], []), []);
});

test("progress in a few characters", () => {
  assert.equal(progressShort(mission("a", "running")), "7/15");
  assert.equal(progressShort(mission("b", "running", { progress: { total: 0, done: 0, blocked: 0, waiting: 0, todo: 0, doing: 0, failed: 0, skipped: 0, left: 0 } })), null);
});
