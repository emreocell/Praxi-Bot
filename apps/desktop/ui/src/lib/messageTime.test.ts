import assert from "node:assert/strict";
import test from "node:test";
import {
  dayHeader,
  formatClock,
  formatDateTime,
  formatStepDuration,
  messageTimeLabel,
  parseMessageTimes
} from "./messageTime.ts";

// local-time constructors: the formatter is local by design
const local = (y: number, mo: number, d: number, h: number, mi: number, s = 0) =>
  new Date(y, mo - 1, d, h, mi, s).getTime();

test("message time label: off / minutes / seconds, tr and en", () => {
  const ts = local(2026, 11, 17, 9, 5, 7);
  assert.equal(messageTimeLabel(ts, "off", "tr"), null);
  const min = messageTimeLabel(ts, "minutes", "tr");
  assert.equal(min?.text, "09:05");
  assert.equal(min?.hover, "09:05:07");
  assert.match(min?.title ?? "", /09:05:07/);
  assert.equal(messageTimeLabel(ts, "seconds", "tr")?.text, "09:05:07");
  assert.match(messageTimeLabel(ts, "minutes", "en")?.text ?? "", /^0?9:05/);
  const done = messageTimeLabel(ts, "minutes", "tr", ts + 12_000);
  assert.match(done?.title ?? "", /09:05:07.*09:05:19/);
  assert.equal(parseMessageTimes("garbage"), "minutes");
  assert.equal(parseMessageTimes("seconds"), "seconds");
});

test("day header: today, yesterday, date; time only when asked", () => {
  const now = local(2026, 11, 17, 21, 20);
  const words = { today: "Bugün", yesterday: "Dün" };
  assert.equal(dayHeader(local(2026, 11, 17, 8, 3), now, "tr", words, false), "Bugün");
  assert.equal(dayHeader(local(2026, 11, 17, 8, 3), now, "tr", words, true), "Bugün 08:03");
  assert.equal(dayHeader(local(2026, 11, 16, 23, 59), now, "tr", words, false), "Dün");
  assert.equal(dayHeader(local(2026, 11, 15, 12, 0), now, "tr", words, false), "15 Kasım");
  assert.equal(dayHeader(local(2025, 3, 2, 12, 0), now, "tr", words, false), "2 Mart 2025");
});

test("step duration: tenths under 10 s, whole seconds, minutes", () => {
  const u = { s: "sn", m: "dk" };
  assert.equal(formatStepDuration(2400, "tr", u), "2,4 sn");
  assert.equal(formatStepDuration(300, "tr", u), "0,3 sn");
  assert.equal(formatStepDuration(12_400, "tr", u), "12 sn");
  assert.equal(formatStepDuration(65_000, "tr", u), "1 dk 5 sn");
  assert.equal(formatStepDuration(120_000, "en", { s: "s", m: "min" }), "2 min");
  assert.equal(formatStepDuration(2400, "en", { s: "s", m: "min" }), "2.4 s");
});

test("dates in sentences follow the app language, tr and de on a 24 h clock (#642)", () => {
  const ts = local(2026, 11, 22, 15, 4, 5);
  assert.equal(formatDateTime(ts, "tr"), "22 Kas 2026 15:04");
  assert.equal(formatDateTime(ts, "de"), "22. Nov. 2026, 15:04");
  assert.equal(formatClock(ts, "tr", true), "15:04:05");
  assert.equal(formatClock(ts, "de", true), "15:04:05");
  // 00:xx stays 00:xx, never "24:xx" or a 12 h clock
  assert.equal(formatDateTime(local(2026, 11, 22, 0, 7), "tr"), "22 Kas 2026 00:07");
  assert.equal(formatDateTime(local(2026, 11, 22, 0, 7), "de"), "22. Nov. 2026, 00:07");
});
