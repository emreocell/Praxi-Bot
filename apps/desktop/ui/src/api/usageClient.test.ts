import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { ApiClient } from "./client.ts";
import { MockApiClient } from "./mock.ts";

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

test("usage and usageExport send range and time zone query params", async () => {
  const seen: Array<{ method: string; url: string }> = [];
  globalThis.fetch = (async (url: URL | string, init?: RequestInit) => {
    seen.push({ method: init?.method ?? "GET", url: String(url) });
    if (String(url).includes("/usage/export")) {
      return new Response("day,bot,model,purpose,calls,calls_without_usage,prompt_tokens,completion_tokens,cached_tokens,cost_usd\n", {
        status: 200,
        headers: { "content-type": "text/csv; charset=utf-8" }
      });
    }
    return new Response(
      JSON.stringify({
        runs: 0,
        total: { prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, model_calls: 0, cost_usd: null },
        by_bot: [],
        by_purpose: {},
        by_model: [],
        by_provider: [],
        daily: [],
        calls_without_usage: 0,
        calls_without_cost: 0,
        time_zone: "UTC"
      }),
      { status: 200 }
    );
  }) as typeof fetch;

  const client = new ApiClient({ base_url: "http://127.0.0.1:9/", token: "tok" });
  await client.usage({ days: 30, tz: "Europe/Istanbul" });
  const csv = await client.usageExport({ days: 30, tz: "Europe/Istanbul" });

  assert.equal(seen.length, 2);
  const u1 = new URL(seen[0].url);
  assert.equal(u1.pathname, "/v1/usage");
  assert.equal(u1.searchParams.get("days"), "30");
  assert.equal(u1.searchParams.get("tz"), "Europe/Istanbul");

  const u2 = new URL(seen[1].url);
  assert.equal(u2.pathname, "/v1/usage/export");
  assert.equal(u2.searchParams.get("days"), "30");
  assert.equal(u2.searchParams.get("tz"), "Europe/Istanbul");
  assert.match(csv, /^day,bot,model,purpose,calls/);
});

test("MockApiClient serves by_model, daily series, and CSV export", async () => {
  const mock = new MockApiClient();
  const summary = await mock.usage({ days: 7, tz: "UTC" });
  assert.equal(summary.daily?.length, 7);
  assert.ok((summary.by_model?.length ?? 0) >= 2);
  assert.equal(summary.time_zone, "UTC");
  const csv = await mock.usageExport({ days: 7, tz: "UTC" });
  assert.match(csv, /^day,bot,model,purpose,calls/);
  assert.match(csv, /anthropic\/claude-sonnet-4-5/);
});
