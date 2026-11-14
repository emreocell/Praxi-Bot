import assert from "node:assert/strict";
import { test } from "node:test";
import { quotaTone, ringDash, visibleQuotas } from "./quota.ts";

const q = (remainingPct: number) => ({ id: "x", brand: "claude", label: "Claude", remainingPct });

test("visibleQuotas hides when there is no data and clamps what is there", () => {
  assert.deepEqual(visibleQuotas(null), []);
  assert.deepEqual(visibleQuotas([]), []);
  assert.deepEqual(visibleQuotas([q(Number.NaN), q(Number.POSITIVE_INFINITY)]), []);
  assert.deepEqual(visibleQuotas([q(120.4), q(-5), q(72.6)]).map((x) => x.remainingPct), [100, 0, 73]);
});

test("ringDash fills a proportional arc of the circle", () => {
  const c = 2 * Math.PI * 20;
  assert.equal(ringDash(0), `0.00 ${c.toFixed(2)}`);
  assert.equal(ringDash(100), `${c.toFixed(2)} ${c.toFixed(2)}`);
  assert.equal(ringDash(50), `${(c / 2).toFixed(2)} ${c.toFixed(2)}`);
  assert.equal(ringDash(900), ringDash(100));
});

test("quotaTone", () => {
  assert.equal(quotaTone(5), "low");
  assert.equal(quotaTone(21), "mid");
  assert.equal(quotaTone(73), "ok");
});
