import assert from "node:assert/strict";
import { test } from "node:test";
import { isTimeoutError, parseShardArg, pickShard, resolveBudgetSeconds } from "./a11y-check-lib.mjs";

test("parseShardArg accepts i/n", () => {
  assert.deepEqual(parseShardArg("1/2"), { index: 1, total: 2 });
  assert.deepEqual(parseShardArg("2/2"), { index: 2, total: 2 });
  assert.equal(parseShardArg(null), null);
});

test("parseShardArg rejects invalid", () => {
  assert.throws(() => parseShardArg("0/2"), /invalid/);
  assert.throws(() => parseShardArg("3/2"), /invalid/);
  assert.throws(() => parseShardArg("x"), /invalid/);
});

test("pickShard splits cases deterministically", () => {
  const cases = ["a", "b", "c", "d", "e"];
  assert.deepEqual(pickShard(cases, { index: 1, total: 2 }), ["a", "c", "e"]);
  assert.deepEqual(pickShard(cases, { index: 2, total: 2 }), ["b", "d"]);
  assert.deepEqual(pickShard(cases, null), cases);
});

test("resolveBudgetSeconds", () => {
  assert.equal(resolveBudgetSeconds(128, { explicit: "200", ci: true }), 200);
  assert.equal(resolveBudgetSeconds(128, { explicit: "", ci: false }), 120);
  assert.equal(resolveBudgetSeconds(128, { explicit: null, ci: true }), Math.ceil(128 * 1.35 + 90));
  assert.equal(resolveBudgetSeconds(10, { explicit: null, ci: false }), 120);
});

test("isTimeoutError", () => {
  assert.equal(isTimeoutError(new Error("locator.click: Timeout 5000ms exceeded.")), true);
  assert.equal(isTimeoutError({ name: "TimeoutError", message: "x" }), true);
  assert.equal(isTimeoutError(new Error("axe violation")), false);
});