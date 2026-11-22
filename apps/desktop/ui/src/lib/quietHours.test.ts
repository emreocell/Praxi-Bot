import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_QUIET_HOURS, isQuiet, parseQuietHours, shouldHold, type QuietHours } from "./quietHours.ts";

const q = (patch: Partial<QuietHours> = {}): QuietHours => ({
  ...DEFAULT_QUIET_HOURS,
  enabled: true,
  timezone: "UTC",
  ...patch
});
const at = (iso: string) => new Date(iso);

test("daytime window: inclusive start, exclusive end, only on chosen days", () => {
  const w = q({ from: "09:00", to: "17:00", days: [1] }); // Mondays
  assert.equal(isQuiet(at("2026-11-23T09:00:00Z"), w), true); // Monday
  assert.equal(isQuiet(at("2026-11-23T16:59:00Z"), w), true);
  assert.equal(isQuiet(at("2026-11-23T17:00:00Z"), w), false);
  assert.equal(isQuiet(at("2026-11-24T10:00:00Z"), w), false); // Tuesday
});

test("overnight window belongs to the day it starts on", () => {
  const w = q({ from: "22:00", to: "07:00", days: [1] }); // Monday night only
  assert.equal(isQuiet(at("2026-11-23T23:00:00Z"), w), true); // Mon 23:00
  assert.equal(isQuiet(at("2026-11-24T03:00:00Z"), w), true); // Tue 03:00 (Monday's window)
  assert.equal(isQuiet(at("2026-11-24T23:00:00Z"), w), false); // Tue 23:00
  assert.equal(isQuiet(at("2026-11-23T03:00:00Z"), w), false); // Mon 03:00 (Sunday's window)
  assert.equal(isQuiet(at("2026-11-24T07:00:00Z"), w), false);
});

test("evaluates wall clock in the chosen zone, DST included", () => {
  const w = q({ from: "22:00", to: "07:00", timezone: "Europe/Berlin" });
  // 2026-03-29 is the spring-forward day (CET to CEST at 01:00Z).
  assert.equal(isQuiet(at("2026-03-29T20:30:00Z"), w), true);
  assert.equal(isQuiet(at("2026-03-29T19:30:00Z"), w), false); // 21:30 CET->CEST: before 22:00
  // Same instant, different offset: 20:30Z is 21:30 CET in winter, 22:30 CEST in summer.
  assert.equal(isQuiet(at("2026-01-15T20:30:00Z"), w), false);
  assert.equal(isQuiet(at("2026-07-15T20:30:00Z"), w), true);
});

test("disabled, no days, or from == to is never quiet", () => {
  const now = at("2026-11-23T23:00:00Z");
  assert.equal(isQuiet(now, q({ enabled: false })), false);
  assert.equal(isQuiet(now, q({ days: [] })), false);
  assert.equal(isQuiet(now, q({ from: "10:00", to: "10:00" })), false);
});

test("urgent approvals break through only when allowed", () => {
  const now = at("2026-11-23T23:00:00Z");
  assert.equal(shouldHold(now, q(), true), false);
  assert.equal(shouldHold(now, q(), false), true);
  assert.equal(shouldHold(now, q({ allowUrgent: false }), true), true);
  assert.equal(shouldHold(at("2026-11-23T12:00:00Z"), q(), false), false);
});

test("parseQuietHours repairs bad stored data", () => {
  const p = parseQuietHours({ enabled: true, from: "25:00", to: "06:30", days: [1, 1, 9, "x", 3], timezone: "Nope/Zone" });
  assert.deepEqual(p, { ...DEFAULT_QUIET_HOURS, enabled: true, to: "06:30", days: [1, 3], timezone: "" });
  assert.deepEqual(parseQuietHours(null), DEFAULT_QUIET_HOURS);
});
