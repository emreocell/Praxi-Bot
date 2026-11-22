import assert from "node:assert/strict";
import { test } from "node:test";
import {
  DEFAULT_SIMPLE,
  simpleProblem,
  simpleSpec,
  zonedWallTimeToMs
} from "./scheduleBuilder.ts";

test("the chips build the cron the daemon expects, with the zone", () => {
  const tz = "Europe/Berlin";
  assert.deepEqual(simpleSpec(DEFAULT_SIMPLE, tz), {
    cron: "0 9 * * * Europe/Berlin"
  });
  assert.deepEqual(
    simpleSpec({ ...DEFAULT_SIMPLE, freq: "weekdays", time: "08:30" }, tz),
    { cron: "30 8 * * 1-5 Europe/Berlin" }
  );
  assert.deepEqual(
    simpleSpec({ ...DEFAULT_SIMPLE, freq: "weekly", weekday: 0 }, tz),
    { cron: "0 9 * * 0 Europe/Berlin" }
  );
  // Days past the 28th are missing in some months, so they are clamped.
  assert.deepEqual(
    simpleSpec({ ...DEFAULT_SIMPLE, freq: "monthly", monthDay: 31 }, tz),
    { cron: "0 9 28 * * Europe/Berlin" }
  );
  assert.equal(simpleSpec({ ...DEFAULT_SIMPLE, time: "25:00" }, tz), null);
  assert.equal(simpleSpec({ ...DEFAULT_SIMPLE, freq: "custom" }, tz), null);
});

test("a one-off time is read in the chosen zone, also across daylight saving", () => {
  // Winter: Istanbul is UTC+3 all year; Berlin is UTC+1 in January, UTC+2 in July.
  assert.equal(
    zonedWallTimeToMs("2027-01-15", "09:00", "Europe/Istanbul"),
    Date.UTC(2027, 0, 15, 6, 0)
  );
  assert.equal(
    zonedWallTimeToMs("2027-01-15", "09:00", "Europe/Berlin"),
    Date.UTC(2027, 0, 15, 8, 0)
  );
  assert.equal(
    zonedWallTimeToMs("2027-07-15", "09:00", "Europe/Berlin"),
    Date.UTC(2027, 6, 15, 7, 0)
  );
});

test("a one-off needs a date that is still ahead", () => {
  const now = Date.UTC(2027, 0, 1);
  const once = { ...DEFAULT_SIMPLE, freq: "once" as const };
  assert.equal(simpleProblem(once, "UTC", now), "date");
  assert.equal(simpleProblem({ ...once, date: "2026-12-31" }, "UTC", now), "past");
  assert.equal(simpleProblem({ ...once, date: "2027-02-01" }, "UTC", now), null);
  assert.deepEqual(simpleSpec({ ...once, date: "2027-02-01" }, "UTC"), {
    at: { unix_ms: Date.UTC(2027, 1, 1, 9, 0) }
  });
});
