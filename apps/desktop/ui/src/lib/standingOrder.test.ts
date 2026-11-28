import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildStandingPayload,
  DEFAULT_MAX_RUNS_PER_DAY,
  lastStandingVerdict,
  validateStanding
} from "./standingOrder.ts";

test("validateStanding accepts a plain checklist and default cap", () => {
  assert.equal(
    validateStanding({
      checklist: "Is mail answered?",
      activeHoursEnabled: false,
      activeHoursStart: "08:00",
      activeHoursEnd: "22:00",
      activeHoursTz: "UTC",
      maxRunsPerDay: DEFAULT_MAX_RUNS_PER_DAY
    }),
    null
  );
});

test("buildStandingPayload omits active hours when disabled", () => {
  assert.deepEqual(
    buildStandingPayload({
      checklist: "  check disks  ",
      activeHoursEnabled: false,
      activeHoursStart: "08:00",
      activeHoursEnd: "22:00",
      activeHoursTz: "Europe/Istanbul",
      maxRunsPerDay: 12
    }),
  {
    kind: "standing_order",
    checklist: "check disks",
    max_runs_per_day: 12
  }
  );
});

test("lastStandingVerdict picks the newest labelled row", () => {
  assert.equal(
    lastStandingVerdict([
      { status: "succeeded", standing_verdict: "ok" },
      { status: "missed" }
    ]),
    "ok"
  );
});