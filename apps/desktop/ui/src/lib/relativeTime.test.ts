import assert from "node:assert/strict";
import test from "node:test";
import { formatRelativeTime } from "./relativeTime.ts";

const t = (key: string, values?: Record<string, unknown>) =>
  values?.count !== undefined ? `${key}:${values.count}` : key;

test("formats seconds, minutes, hours, yesterday, and older dates", () => {
  const now = new Date(2026, 10, 24, 15, 30, 0).getTime();
  assert.equal(formatRelativeTime(now - 10_000, now, "en", t), "notifyHistory.time.justNow");
  assert.equal(formatRelativeTime(now - 2 * 60_000, now, "en", t), "notifyHistory.time.minutesAgo:2");
  assert.equal(formatRelativeTime(now - 3 * 3_600_000, now, "en", t), "notifyHistory.time.hoursAgo:3");
  const yesterday = new Date(2026, 10, 23, 18, 0, 0).getTime();
  assert.equal(formatRelativeTime(yesterday, now, "en", t), "notifyHistory.time.yesterday");
  const older = new Date(2026, 9, 5, 12, 0, 0).getTime();
  const formatted = formatRelativeTime(older, now, "en-US", t);
  assert.match(formatted, /Oct\s+5/);
});
