import assert from "node:assert/strict";
import { test } from "node:test";
import { paceOf, parseLimitInput } from "./spendLimit.ts";

const base = {
  month_start_ms: 0, month_end_ms: 1, now_ms: 0, unpriced_calls: 0,
  limit_usd: 100, remaining_usd: 100, spent_usd: 0, forecast_usd: null
};

test("the limit form rejects amounts under $1 and junk, accepts a comma decimal", () => {
  assert.equal(parseLimitInput("0.5"), null);
  assert.equal(parseLimitInput(""), null);
  assert.equal(parseLimitInput("abc"), null);
  assert.equal(parseLimitInput("25,5"), 25.5);
  assert.equal(parseLimitInput(" 40 "), 40);
});

test("a thousands separator is rejected instead of being read as a decimal point", () => {
  assert.equal(parseLimitInput("1.000"), null);
  assert.equal(parseLimitInput("1,000"), null);
  assert.equal(parseLimitInput("2.500"), null);
  assert.equal(parseLimitInput("1000"), 1000);
  assert.equal(parseLimitInput("12.5"), 12.5);
  assert.equal(parseLimitInput("1000000"), 1_000_000);
  assert.equal(parseLimitInput("1000001"), null);
});

test("pace says off without a limit, reached/warn from spend, over/under from the forecast", () => {
  assert.equal(paceOf({ ...base, limit_usd: null }), "off");
  assert.equal(paceOf({ ...base, spent_usd: 100 }), "reached");
  assert.equal(paceOf({ ...base, spent_usd: 80 }), "warn");
  assert.equal(paceOf({ ...base, spent_usd: 10 }), "noForecast");
  assert.equal(paceOf({ ...base, spent_usd: 10, forecast_usd: 130 }), "over");
  assert.equal(paceOf({ ...base, spent_usd: 10, forecast_usd: 60 }), "under");
});
