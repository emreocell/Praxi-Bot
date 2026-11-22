import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_QUIET_HOURS, type QuietHours } from "./quietHours.ts";
import { dayKey, digestText, parseDigestTime, planDigest } from "./digestNotify.ts";
import type { BotDigest } from "./dailyDigest.ts";

const at = (h: number, m = 0) => new Date(2026, 10, 22, h, m);
const quiet: QuietHours = { ...DEFAULT_QUIET_HOURS, enabled: true, from: "22:00", to: "07:00" };
const off: QuietHours = DEFAULT_QUIET_HOURS;

test("digest is off unless a time is set, and only fires once a day", () => {
  assert.equal(planDigest(at(20), "", "", off), "wait");
  assert.equal(planDigest(at(19, 59), "20:00", "", off), "wait");
  assert.equal(planDigest(at(20, 1), "20:00", "", off), "show");
  assert.equal(planDigest(at(20, 1), "20:00", dayKey(at(20)), off), "wait");
  assert.equal(planDigest(at(20, 1), "20:00", "2026-11-21", off), "show");
});

test("quiet hours hold the digest instead of showing it", () => {
  assert.equal(planDigest(at(22, 30), "22:15", "", quiet), "hold");
  assert.equal(planDigest(at(22, 30), "22:15", "", off), "show");
});

test("a digest long past its time is skipped, not delivered late", () => {
  assert.equal(planDigest(at(23, 0), "08:00", "", off), "skip");
});

test("parseDigestTime accepts HH:MM only", () => {
  assert.equal(parseDigestTime("08:30"), "08:30");
  assert.equal(parseDigestTime("8:30"), "");
  assert.equal(parseDigestTime(42), "");
});

const row = (over: Partial<BotDigest>): BotDigest => ({
  botId: "b", done: 0, failed: 0, stopped: 0, waiting: 0, running: 0, calls: 0, tokens: 0, costUsd: null, limitUsed: null, ...over
});
const t = (k: string, v?: Record<string, unknown>) => `${k}${v ? JSON.stringify(v) : ""}`;

test("digest text sums bots, never names a bot, and is generic in private mode", () => {
  const rows = [row({ botId: "SECRETBOT", done: 2, costUsd: 0.5 }), row({ failed: 1, costUsd: 0.25 })];
  const body = digestText(rows, { private: false, cost: (n) => `$${n}` }, t);
  assert.ok(body && body.body.includes('"count":2') && body.body.includes("$0.75"));
  assert.ok(!JSON.stringify(body).includes("SECRETBOT"));
  assert.equal(digestText(rows, { private: true, cost: String }, t)?.body, "digest.notifyPrivate");
  assert.equal(digestText([], { private: false, cost: String }, t), null);
});
