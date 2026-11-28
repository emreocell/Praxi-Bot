import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { Bot, BotSpendLimits, Run } from "../api/types.ts";
import type { ControlSnapshot } from "../overlay/events.ts";
import type { EventsState, NeedsYouEntry } from "../state/timeline.ts";
import { dailyBudget, nativePauseAction, overviewBots, selectOverviewBots } from "./myBots.ts";

const bot = (id: string, name = id): Bot => ({ id, name, config: {}, created_at_ms: 1, version: 1 });
const run = (id: string, botId: string, state: string, at = 10): Run => ({ id, bot_id: botId, state, kind: "turn", created_at_ms: at, updated_at_ms: at });
const empty: EventsState = { runs: {}, lastGseq: 0 };
const native = (id: string, mode: ControlSnapshot["mode"]) => ({ bot: { botId: id }, mode }) as ControlSnapshot;
const spend = (usd: number, limits = {}): BotSpendLimits => ({
  usage: { daily: { usd } }, limits, is_default: false, default_daily_hard_usd: 5
}) as BotSpendLimits;

test("every bot appears, with approval and pause states from the current facts", () => {
  const rows = overviewBots([bot("idle"), bot("working"), bot("approval"), bot("paused")], [
    run("r1", "working", "queued"), run("r2", "approval", "waiting_approval"), run("r3", "paused", "paused")
  ], empty, [], null);
  assert.deepEqual(rows.map((r) => r.status), ["idle", "running", "approval", "paused"]);
  assert.equal(rows[0].lastActivity, null);
  assert.equal(rows[3].pausedRunId, "r3");
});

test("live starts and finishes update status before the list catches up; requests link to their chat", () => {
  const events: EventsState = { runs: { r: { id: "r", botId: "a", chatId: "chat", startedMs: 10, items: [{ kind: "text", key: "text", text: "untrusted", ts: 25 }] } }, lastGseq: 1 };
  assert.equal(overviewBots([bot("a")], [], events, [], null)[0].status, "running");
  const needs = [{ botId: "a", chatId: "chat", runId: "r" }] as NeedsYouEntry[];
  const waiting = overviewBots([bot("a")], [], events, needs, null)[0];
  assert.equal(waiting.status, "approval");
  assert.equal(waiting.needsChatId, "chat");
  assert.equal(waiting.lastActivity, 25);
  events.runs.r.finishedMs = 30;
  const finished = overviewBots([bot("a")], [run("r", "a", "running")], events, [], null)[0];
  assert.equal(finished.status, "idle");
  assert.equal(finished.lastActivity, 30);
  assert.equal(overviewBots([bot("a")], [run("r", "a", "succeeded")], { runs: { r: { ...events.runs.r, finishedMs: undefined } }, lastGseq: 1 }, [], null)[0].status, "idle");
});

test("search, status and sorting work together without mutating input or treating unknown cost as zero", () => {
  const rows = overviewBots([bot("z", "İpek"), bot("a", "Atlas"), bot("b", "Atlas 2")], [run("r", "a", "running", 20)], empty, [], null);
  assert.deepEqual(selectOverviewBots(rows, "ipek", "idle", "name", {}, "tr").map((r) => r.bot.id), ["z"]);
  assert.deepEqual(selectOverviewBots(rows, "Atlas", "running", "activity", {}, "en").map((r) => r.bot.id), ["a"]);
  assert.deepEqual(selectOverviewBots(rows, "", "all", "cost", { a: spend(0), b: spend(3) }, "en").map((r) => r.bot.id), ["b", "a", "z"]);
  assert.deepEqual(rows.map((r) => r.bot.id), ["z", "a", "b"]);
});

test("daily cost uses the daemon's default or saved hard limit, never a soft or monthly cap", () => {
  const fallback = { ...spend(1), is_default: true };
  assert.deepEqual(dailyBudget(fallback), { used: 1, limit: 5 });
  assert.deepEqual(dailyBudget(spend(2, { daily: { soft_usd: 3, hard_usd: 8 } })), { used: 2, limit: 8 });
  assert.deepEqual(dailyBudget(spend(0, { monthly: { hard_usd: 100 } })), { used: 0, limit: null });
});

test("native pause belongs to the local bot controlling the computer; concurrent work stays running", () => {
  assert.equal(nativePauseAction("a", true, native("a", "bot_driving")), "pause");
  assert.equal(nativePauseAction("a", true, native("a", "paused")), "resume");
  assert.equal(nativePauseAction("b", true, native("a", "bot_driving")), null);
  assert.equal(nativePauseAction("a", false, native("a", "bot_driving")), null);
  assert.equal(nativePauseAction("a", true, native("a", "human_driving")), null);
  const rows = overviewBots([bot("a")], [run("r", "a", "running"), run("p", "a", "paused")], empty, [], null);
  assert.equal(rows[0].status, "running");
  assert.equal(rows[0].pausedRunId, undefined);
});

test("overview entry points and bot-specific settings and schedule actions stay connected", () => {
  const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
  const app = read("../App.tsx");
  assert.match(app, /data-testid="my-bots-button"[^\n]+go\(\{ kind: "myBots"/);
  assert.match(app, /<MyBotsView/);
  assert.match(app, /onSettings=\{\(botId\) => go\(\{ kind: "bot", botId, edit: true/);
  assert.match(app, /onSchedule=\{\(botId\) => go\(\{ kind: "schedules", botId/);
  assert.match(read("../state/PaletteHost.tsx"), /const SCREENS[\s\S]+"myBots"/);
  assert.match(read("../views/SchedulesView.tsx"), /emptyDraft\([\s\S]*initialBotId/);
});
