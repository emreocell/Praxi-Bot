import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildDigest,
  digestBucket,
  limitUsed,
  startOfLocalDay,
  type DigestLimit
} from "./dailyDigest.ts";

const DAY = 1_000_000;
const run = (bot_id: string, state: string, at = DAY + 10) => ({ bot_id, state, created_at_ms: at });
const none = new Map<string, DigestLimit>();

test("startOfLocalDay is local midnight", () => {
  const d = new Date(2026, 10, 22, 17, 45, 12);
  const start = new Date(startOfLocalDay(d));
  assert.equal(start.getHours(), 0);
  assert.equal(start.getMinutes(), 0);
  assert.equal(start.getDate(), 22);
});

test("digestBucket maps run states to plain groups", () => {
  assert.equal(digestBucket("succeeded"), "done");
  assert.equal(digestBucket("failed"), "failed");
  assert.equal(digestBucket("cancelled"), "stopped");
  assert.equal(digestBucket("paused"), "waiting");
  assert.equal(digestBucket("waiting_approval"), "waiting");
  assert.equal(digestBucket("running"), "running");
  assert.equal(digestBucket("something-new"), "running");
});

test("buildDigest counts only today's runs per bot", () => {
  const out = buildDigest({
    runs: [run("a", "succeeded"), run("a", "failed"), run("a", "succeeded", DAY - 1), run("b", "cancelled")],
    usage: [],
    limits: none,
    dayStartMs: DAY
  });
  const a = out.find((e) => e.botId === "a");
  assert.deepEqual([a?.done, a?.failed], [1, 1]);
  assert.equal(out.find((e) => e.botId === "b")?.stopped, 1);
});

test("buildDigest adds usage, skips idle bots and sorts attention first", () => {
  const out = buildDigest({
    runs: [run("busy", "succeeded"), run("busy", "succeeded"), run("blocked", "paused")],
    usage: [
      { bot_id: "busy", model_calls: 5, prompt_tokens: 100, completion_tokens: 50, cost_usd: 0.25 },
      { bot_id: "idle", model_calls: 0, prompt_tokens: 0, completion_tokens: 0, cost_usd: null },
      { bot_id: "chatty", model_calls: 2, prompt_tokens: 10, completion_tokens: 10 }
    ],
    limits: none,
    dayStartMs: DAY
  });
  assert.deepEqual(
    out.map((e) => e.botId),
    ["blocked", "busy", "chatty"]
  );
  const busy = out.find((e) => e.botId === "busy");
  assert.equal(busy?.calls, 5);
  assert.equal(busy?.tokens, 150);
  assert.equal(busy?.costUsd, 0.25);
  assert.equal(out.find((e) => e.botId === "chatty")?.costUsd, null);
});

test("limitUsed takes the larger share and is null without a limit", () => {
  assert.equal(limitUsed(undefined, 1, 100), null);
  assert.equal(limitUsed({ hardUsd: null, hardTokens: null }, 1, 100), null);
  assert.equal(limitUsed({ hardUsd: 2, hardTokens: null }, 0.5, 100), 0.25);
  assert.equal(limitUsed({ hardUsd: 2, hardTokens: 100 }, 0.5, 80), 0.8);
  // an unknown price does not count as zero cost against a money limit
  assert.equal(limitUsed({ hardUsd: 2, hardTokens: null }, null, 100), null);
});

test("buildDigest attaches the limit share per bot", () => {
  const out = buildDigest({
    runs: [run("a", "succeeded")],
    usage: [{ bot_id: "a", model_calls: 1, prompt_tokens: 1, completion_tokens: 1, cost_usd: 1 }],
    limits: new Map([["a", { hardUsd: 4, hardTokens: null }]]),
    dayStartMs: DAY
  });
  assert.equal(out[0].limitUsed, 0.25);
});
