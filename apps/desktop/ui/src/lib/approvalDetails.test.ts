import assert from "node:assert/strict";
import { test } from "node:test";
import { formatFiring, scheduleDetails } from "./approvalDetails.ts";

const card = {
  kind: "schedule",
  action: "create",
  name: "Standup",
  prompt: "Summarise my inbox.",
  schedule: "Every weekday at 10:30",
  timezone: "Europe/Istanbul",
  next_runs: [
    "2026-11-13T10:30:00+03:00",
    "2026-11-16T10:30:00+03:00",
    "2026-11-17T10:30:00+03:00"
  ],
  permission: { level: "ask_first", allow: [], ask: ["fs", "shell"], deny: ["money"] }
};

test("a schedule card is parsed with the prompt, timing, firings and permission", () => {
  const d = scheduleDetails(card);
  assert.ok(d);
  assert.equal(d.action, "create");
  assert.equal(d.prompt, "Summarise my inbox.");
  assert.equal(d.schedule, "Every weekday at 10:30");
  assert.equal(d.timezone, "Europe/Istanbul");
  assert.equal(d.nextRuns.length, 3);
  assert.equal(d.permission?.level, "ask_first");
  assert.deepEqual(d.permission?.deny, ["money"]);
});

test("anything else is not a schedule card", () => {
  assert.equal(scheduleDetails(undefined), null);
  assert.equal(scheduleDetails(null), null);
  assert.equal(scheduleDetails("x"), null);
  assert.equal(scheduleDetails({ kind: "other" }), null);
  assert.equal(scheduleDetails({ ...card, action: "delete" }), null);
});

test("missing or malformed fields degrade instead of throwing", () => {
  const d = scheduleDetails({ kind: "schedule", action: "resume", next_runs: [1, "a"] });
  assert.ok(d);
  assert.equal(d.prompt, "");
  assert.deepEqual(d.nextRuns, ["a"]);
  assert.equal(d.permission, null);
});

test("a firing keeps the wall-clock time of its own offset", () => {
  assert.match(formatFiring("2026-11-13T10:30:00+03:00", "en"), /10:30$/);
  assert.match(formatFiring("2026-11-13T09:00:00-05:00", "en"), /09:00$/);
  assert.equal(formatFiring("garbage"), "garbage");
});
