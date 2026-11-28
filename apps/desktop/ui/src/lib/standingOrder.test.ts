import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildStandingPayload,
  DEFAULT_MAX_RUNS_PER_DAY,
  lastStandingVerdict,
  MAX_CHECKLIST_CHARS,
  validateStanding
} from "./standingOrder.ts";

const base = {
  activeHoursEnabled: true,
  activeHoursStart: "08:00",
  activeHoursEnd: "22:00",
  activeHoursTz: "Europe/Istanbul",
  maxRunsPerDay: DEFAULT_MAX_RUNS_PER_DAY
};

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

test("validateStanding rejects non-integer caps and long checklists", () => {
  assert.equal(validateStanding({ ...base, checklist: "ok", maxRunsPerDay: 1.5 }), "maxRuns");
  assert.equal(
    validateStanding({ ...base, checklist: "x".repeat(MAX_CHECKLIST_CHARS + 1) }),
    "checklist"
  );
});

test("validateStanding trims times and rejects unknown zones", () => {
  assert.equal(
    validateStanding({ ...base, checklist: "ok", activeHoursStart: "08:00 ", activeHoursEnd: "08:00" }),
    "hoursEnd"
  );
  assert.equal(
    validateStanding({ ...base, checklist: "ok", activeHoursTz: "Nowhere/Land" }),
    "hoursTz"
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