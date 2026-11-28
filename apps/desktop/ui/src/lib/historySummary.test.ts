import assert from "node:assert/strict";
import test from "node:test";
import { historySummaryOn, historySummaryOps } from "./historySummary.ts";

test("the summary is on unless the owner switched it off", () => {
  assert.equal(historySummaryOn({}), true);
  assert.equal(historySummaryOn(null), true);
  assert.equal(historySummaryOn({ history: {} }), true);
  assert.equal(historySummaryOn({ history: { summary: true } }), true);
  assert.equal(historySummaryOn({ history: { summary: false } }), false);
  // Anything that is not an explicit `false` leaves the default.
  assert.equal(historySummaryOn({ history: { summary: "off" } }), true);
});

test("switching it off sets one field, switching it on drops that field", () => {
  assert.deepEqual(historySummaryOps(false), [{ op: "set", path: "/history/summary", value: false }]);
  assert.deepEqual(historySummaryOps(true), [{ op: "unset", path: "/history/summary" }]);
});
