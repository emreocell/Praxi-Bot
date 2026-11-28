import assert from "node:assert/strict";
import { test } from "node:test";
import type { Schedule, ScheduleHistoryEntry } from "../api/types";
import { botAutomationRows, confirmableAuthor, dayWord, lastOutcome, localSpecLabel, primaryAction, scheduleKind } from "./automations.ts";
import { newSource, newTrigger, type BotTrigger } from "./triggers.ts";

const schedule = (id: string, extra: Partial<Schedule> = {}): Schedule =>
  ({
    id, bot_id: "a", name: id, enabled: true, has_webhook: false, created_at_ms: 0, misfire_policy: "skip",
    origin: "user", author: "owner", revision: 1, payload: { kind: "agent_prompt", text: "x" }, spec: { cron: "0 9 * * *" },
    next_run_at_ms: 1000, ...extra
  }) as Schedule;
const trigger = (id: string, origin: BotTrigger["origin"], kind: "mail" | "webhook" | "time" | "feed" | "home_assistant" = "mail"): BotTrigger => {
  const base = newTrigger();
  const source = kind === "mail" ? base.config.source : kind === "webhook" ? { kind: "webhook" as const, id: "h" } : kind === "time" ? { kind: "time" as const, spec: { cron: "0 9 * * *" } } : newSource(kind);
  return { ...base, config: { ...base.config, source }, name: id, origin, author: "owner", revision: 1, id, bot_id: "a", last_fired_at_ms: null, count_today: 0, queued: 0, last_error: null };
};
const entry = (status: string, extra: Partial<ScheduleHistoryEntry> = {}): ScheduleHistoryEntry =>
  ({ due_ms: 500, run_id: null, chat_id: null, status, started_at_ms: null, finished_at_ms: null, error: null, retryable: false, ...extra }) as ScheduleHistoryEntry;

test("one list per bot: its schedules first, then its rules, never a schedule twice", () => {
  const rows = botAutomationRows(
    "a",
    [schedule("later", { next_run_at_ms: 9000 }), schedule("soon", { next_run_at_ms: 10 }), schedule("other", { bot_id: "b" })],
    [trigger("mail-rule", "rule"), trigger("soon", "schedule", "time"), trigger("hook", "webhook", "webhook")]
  );
  assert.deepEqual(rows.map((r) => r.key), ["schedule:soon", "schedule:later", "trigger:mail-rule", "trigger:hook"]);
  assert.deepEqual(rows.map((r) => r.kind), ["time", "time", "mail", "webhook"]);
});

test("rules on a feed or a home device are listed by their own kind", () => {
  const rows = botAutomationRows("a", [], [trigger("news", "rule", "feed"), trigger("door", "rule", "home_assistant")]);
  assert.deepEqual(rows.map((r) => r.kind), ["feed", "home_assistant"]);
});

test("a schedule's kind follows its event trigger", () => {
  assert.equal(scheduleKind({ trigger: { kind: "file_watch", path: "/x" }, has_webhook: false }), "file");
  assert.equal(scheduleKind({ trigger: { kind: "email", account: "a" }, has_webhook: false }), "mail");
  assert.equal(scheduleKind({ trigger: null, has_webhook: true }), "time");
});

test("the last result is the newest history entry, in four tones", () => {
  assert.equal(lastOutcome([]), null);
  assert.deepEqual(
    [entry("succeeded"), entry("failed"), entry("missed"), entry("running")].map((e) => lastOutcome([e])?.tone),
    ["ok", "failed", "missed", "running"]
  );
  assert.equal(lastOutcome([entry("failed", { started_at_ms: 700 })])?.atMs, 700);
  assert.equal(lastOutcome([entry("missed")])?.atMs, 500);
});

test("near days read as words", () => {
  const now = new Date(2026, 10, 27, 23, 30).getTime();
  assert.equal(dayWord(new Date(2026, 10, 27, 0, 5).getTime(), now), "today");
  assert.equal(dayWord(new Date(2026, 10, 28, 9, 0).getTime(), now), "tomorrow");
  assert.equal(dayWord(new Date(2026, 10, 26, 9, 0).getTime(), now), "yesterday");
  assert.equal(dayWord(new Date(2026, 11, 1, 9, 0).getTime(), now), null);
});

test("one primary action per schedule row", () => {
  assert.deepEqual(primaryAction({ moved_to: "Server" }, lastOutcome([entry("failed", { retryable: true })])), { kind: "reclaim" });
  assert.deepEqual(primaryAction({ moved_to: null }, lastOutcome([entry("missed", { retryable: true })])), { kind: "retry", dueMs: 500 });
  // A failure that cannot be retried (a direct run) falls back to running it again.
  assert.deepEqual(primaryAction({ moved_to: null }, lastOutcome([entry("failed", { due_ms: null })])), { kind: "runNow" });
  assert.deepEqual(primaryAction({ moved_to: null }, lastOutcome([entry("succeeded", { retryable: true })])), { kind: "runNow" });
  assert.deepEqual(primaryAction({}, null), { kind: "runNow" });
});

test("the local time zone is left out of the 'when' line, another one stays", () => {
  const fmt = (ms: number) => String(ms);
  assert.equal(localSpecLabel({ cron: "0 9 * * 1-5 Europe/Istanbul" }, fmt, "en", "Europe/Istanbul").values.zone, "");
  assert.equal(localSpecLabel({ cron: "0 9 * * 1-5 America/New_York" }, fmt, "en", "Europe/Istanbul").values.zone, " (America/New_York)");
  assert.equal(localSpecLabel({ every: { seconds: 3600 } }, fmt, "en", "UTC").i18nKey, "schedules.spec.every.hours");
});

test("only an unconfirmed user-side schedule or stored rule on a Full bot offers the one-click confirm", () => {
  const row = (s: Schedule) => botAutomationRows("a", [s], [])[0]!;
  const rule = (extra: Partial<BotTrigger>) => botAutomationRows("a", [], [{ ...trigger("r", "rule"), ...extra }])[0]!;
  assert.equal(confirmableAuthor(row(schedule("s", { author: "unknown" })), true), true);
  assert.equal(confirmableAuthor(rule({ author: "unknown" }), true), true);
  // not on Full, already confirmed, bot-made (editing takes it over), moved away, or only an adoptable source
  assert.equal(confirmableAuthor(row(schedule("s", { author: "unknown" })), false), false);
  assert.equal(confirmableAuthor(row(schedule("s", { author: "owner" })), true), false);
  assert.equal(confirmableAuthor(row(schedule("s", { origin: "bot", author: "bot" })), true), false);
  assert.equal(confirmableAuthor(row(schedule("s", { author: "unknown", moved_to: "Server" })), true), false);
  assert.equal(confirmableAuthor(rule({ author: "bot" }), true), false);
  assert.equal(confirmableAuthor(rule({ author: "unknown", origin: "webhook" }), true), false);
});
