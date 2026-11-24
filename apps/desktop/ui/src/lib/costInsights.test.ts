import assert from "node:assert/strict";
import { test } from "node:test";
import { groupDaily, limitOverlay, periodStart, shareRows } from "./costInsights.ts";
import { buildDailyBars } from "./usage.ts";

const day = (d: string, calls: number, cost: number | null, noCost = 0) => ({
  day: d,
  model_calls: calls,
  prompt_tokens: calls * 100,
  completion_tokens: calls * 10,
  cached_tokens: 0,
  cost_usd: cost,
  calls_without_usage: 0,
  calls_without_cost: noCost
});

test("weeks start on Monday and months on the 1st", () => {
  assert.equal(periodStart("2026-11-22", "week"), "2026-11-16"); // a Sunday
  assert.equal(periodStart("2026-11-16", "week"), "2026-11-16");
  assert.equal(periodStart("2026-11-24", "month"), "2026-11-01");
});

test("grouping sums days and keeps an all-unknown price unknown, never $0", () => {
  const daily = [
    day("2026-11-15", 2, null, 2),
    day("2026-11-16", 1, 0.5),
    day("2026-11-17", 1, null, 1),
    day("2026-11-18", 0, null)
  ];
  const weeks = groupDaily(daily, "week");
  assert.equal(weeks.length, 2);
  assert.equal(weeks[0].cost_usd, null);
  assert.equal(weeks[0].model_calls, 2);
  assert.equal(weeks[1].cost_usd, 0.5);
  assert.equal(weeks[1].calls_without_cost, 1);
  assert.equal(weeks[1].span, 3);
  assert.equal(groupDaily(daily, "month").length, 1);
});

test("limit overlay is exact for a month and the even share for a day", () => {
  assert.equal(limitOverlay({ limit_usd: null }, "month", "2026-11-01"), null);
  const month = limitOverlay({ limit_usd: 30 }, "month", "2026-11-01");
  assert.deepEqual(month && [month.limit, month.alert, month.prorated], [30, 24, false]);
  const d = limitOverlay({ limit_usd: 30 }, "day", "2026-11-01"); // November has 30 days
  assert.deepEqual(d && [d.limit, d.prorated], [1, true]);
});

test("a limit above the data stays on the chart scale", () => {
  const bars = buildDailyBars([day("2026-11-01", 1, 2)], "cost", 100, 100, 10);
  assert.equal(bars.max, 10);
  assert.equal(bars.bars[0].height, 20);
});

test("unknown values rank last and get no bar", () => {
  const rows = shareRows(
    [
      { key: "local", totals: { prompt_tokens: 9, completion_tokens: 1, cached_tokens: 0, model_calls: 3, cost_usd: null } },
      { key: "big", totals: { prompt_tokens: 90, completion_tokens: 10, cached_tokens: 0, model_calls: 3, cost_usd: 2 } },
      { key: "small", totals: { prompt_tokens: 9, completion_tokens: 1, cached_tokens: 0, model_calls: 1, cost_usd: 0.5 } }
    ],
    "cost"
  );
  assert.deepEqual(rows.map((r) => r.key), ["big", "small", "local"]);
  assert.equal(rows[0].fraction, 1);
  assert.equal(rows[2].fraction, null);
});
