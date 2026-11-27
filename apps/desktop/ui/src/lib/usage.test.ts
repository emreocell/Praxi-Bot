import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildDailyBars,
  exportFileName,
  formatTokenCount,
  formatUsageCost,
  isAllUnknownUsage,
  pickLabelIndices,
  shortDayLabel,
  sortModelUsage,
  totalTokens,
  usageQueryForRange,
  topModelsSummary
} from "./usage.ts";

test("usageQueryForRange maps 7/30/90/all presets with time zone", () => {
  assert.deepEqual(usageQueryForRange("week", "Europe/Istanbul"), {
    days: 7,
    tz: "Europe/Istanbul"
  });
  assert.deepEqual(usageQueryForRange("month", "UTC"), { days: 30, tz: "UTC" });
  assert.deepEqual(usageQueryForRange("quarter", "America/New_York"), {
    days: 90,
    tz: "America/New_York"
  });
  assert.deepEqual(usageQueryForRange("all", "UTC"), { tz: "UTC" });
});

test("exportFileName formats range and local calendar date", () => {
  const fixed = new Date(2026, 10, 21, 14, 30, 0);
  assert.equal(exportFileName("week", fixed), "praxi-usage-7d-2026-11-21.csv");
  assert.equal(exportFileName("month", fixed), "praxi-usage-30d-2026-11-21.csv");
  assert.equal(exportFileName("quarter", fixed), "praxi-usage-90d-2026-11-21.csv");
  assert.equal(exportFileName("all", fixed), "praxi-usage-all-2026-11-21.csv");
});

test("unknown cost and unknown tokens are never displayed as zero", () => {
  assert.equal(formatUsageCost(null, "en", "Unknown"), "Unknown");
  assert.equal(formatUsageCost(undefined, "en", "Unknown"), "Unknown");
  assert.match(formatUsageCost(0.125, "en", "Unknown"), /0\.125/);
  assert.equal(formatTokenCount(0, true, "en", "Unknown"), "Unknown");
  assert.equal(formatTokenCount(0, false, "en", "Unknown"), "0");
  assert.equal(isAllUnknownUsage({ model_calls: 2, calls_without_usage: 2 }), true);
  assert.equal(isAllUnknownUsage({ model_calls: 2, calls_without_usage: 1 }), false);
  assert.equal(isAllUnknownUsage({ model_calls: 0, calls_without_usage: 0 }), false);
});

test("sortModelUsage orders by known cost desc, then tokens, calls and name", () => {
  const sorted = sortModelUsage([
    { model: "ollama/llama3", prompt_tokens: 5000, completion_tokens: 500, cached_tokens: 0, model_calls: 10, cost_usd: null },
    { model: "openai/gpt-4o-mini", prompt_tokens: 1000, completion_tokens: 200, cached_tokens: 0, model_calls: 4, cost_usd: 0.01 },
    { model: "anthropic/claude-sonnet", prompt_tokens: 800, completion_tokens: 100, cached_tokens: 0, model_calls: 2, cost_usd: 0.25 }
  ]);
  assert.deepEqual(
    sorted.map((m) => m.model),
    ["anthropic/claude-sonnet", "openai/gpt-4o-mini", "ollama/llama3"]
  );
  assert.equal(totalTokens(sorted[0]), 900);
});

test("buildDailyBars handles empty series, known peaks, and unknown-only days", () => {
  assert.deepEqual(buildDailyBars([], "tokens"), {
    bars: [],
    max: 0,
    hasData: false,
    labelIndices: []
  });

  const series = [
    { day: "2026-11-19", prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, model_calls: 0, cost_usd: null, calls_without_usage: 0, calls_without_cost: 0 },
    { day: "2026-11-20", prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, model_calls: 2, cost_usd: null, calls_without_usage: 2, calls_without_cost: 2 },
    { day: "2026-11-21", prompt_tokens: 800, completion_tokens: 200, cached_tokens: 100, model_calls: 3, cost_usd: 0.05, calls_without_usage: 0, calls_without_cost: 0 }
  ];

  const byTokens = buildDailyBars(series, "tokens", 300, 100);
  assert.equal(byTokens.hasData, true);
  assert.equal(byTokens.max, 1000);
  assert.equal(byTokens.bars[0].height, 0);
  assert.equal(byTokens.bars[1].unknownOnly, true);
  assert.ok(byTokens.bars[1].height > 0);
  assert.equal(byTokens.bars[2].height, 100);

  const byCost = buildDailyBars(series, "cost", 300, 100);
  assert.equal(byCost.bars[1].unknownOnly, true);
  assert.equal(byCost.bars[2].height, 100);

  assert.deepEqual(pickLabelIndices(7, 5), [0, 2, 3, 5, 6]);
  assert.deepEqual(pickLabelIndices(30, 5), [0, 7, 15, 22, 29]);
  assert.equal(shortDayLabel("2026-11-21", "en-US"), "Nov 21");
});

test("the usage summary keeps the top models by cost and says how many more there are", () => {
  const row = (model: string, calls: number, cost: number | null) => ({
    model, model_calls: calls, prompt_tokens: calls * 10, completion_tokens: calls, cached_tokens: 0, cost_usd: cost
  });
  const out = topModelsSummary([row("a", 5, 0.5), row("b", 50, 2), row("c", 9, null), row("d", 0, null), row("e", 3, 1)], 3);
  assert.deepEqual(out.rows.map((r) => r.model), ["b", "e", "a"]);
  assert.equal(out.rows[0].share, 1);
  assert.equal(out.rows[1].share, 0.5);
  // "d" had no calls in the range: it is not "one more model".
  assert.equal(out.more, 1);
  // Nothing priced: the bars compare calls.
  const unpriced = topModelsSummary([row("x", 10, null), row("y", 5, null)], 3);
  assert.deepEqual(unpriced.rows.map((r) => r.share), [1, 0.5]);
  assert.equal(unpriced.more, 0);
});
