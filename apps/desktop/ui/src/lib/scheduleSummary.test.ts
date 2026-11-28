import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_SIMPLE, type SimpleSchedule } from "./scheduleBuilder.ts";
import { weekdayName, whenSummary, zoneCity } from "./scheduleSummary.ts";

const at = (p: Partial<SimpleSchedule>): SimpleSchedule => ({ ...DEFAULT_SIMPLE, ...p });

test("a zone reads as its city", () => {
  assert.equal(zoneCity("Europe/Istanbul"), "Istanbul");
  assert.equal(zoneCity("America/Argentina/Buenos_Aires"), "Buenos Aires");
  assert.equal(zoneCity("UTC"), "UTC");
  assert.equal(zoneCity(""), "UTC");
});

test("every preset reads as one sentence with the time and the city", () => {
  assert.deepEqual(whenSummary(at({ freq: "weekdays", time: "9:00" }), "Europe/Istanbul", "en"), {
    key: "scheduleEditor.summary.weekdays",
    values: { time: "09:00", zone: "Istanbul" }
  });
  assert.equal(whenSummary(at({ freq: "daily" }), "Europe/Berlin", "en")?.key, "scheduleEditor.summary.daily");
  assert.deepEqual(whenSummary(at({ freq: "weekly", weekday: 1 }), "UTC", "en")?.values, { time: "09:00", zone: "UTC", weekday: "Monday" });
  assert.deepEqual(whenSummary(at({ freq: "monthly", monthDay: 15, time: "18:30" }), "UTC", "en")?.values, { time: "18:30", zone: "UTC", day: 15 });
});

test("a one-off names its day, or asks for one", () => {
  const once = whenSummary(at({ freq: "once", date: "2026-11-28" }), "Europe/Istanbul", "en");
  assert.equal(once?.key, "scheduleEditor.summary.once");
  assert.match(String(once?.values.date), /28/);
  assert.equal(whenSummary(at({ freq: "once", date: "" }), "Europe/Istanbul", "en")?.key, "scheduleEditor.summary.onceNoDate");
});

test("no sentence for words (the daemon's reading is shown) or a time that is not HH:MM", () => {
  assert.equal(whenSummary(at({ freq: "custom" }), "UTC", "en"), null);
  assert.equal(whenSummary(at({ time: "25:00" }), "UTC", "en"), null);
  assert.equal(whenSummary(at({ time: "" }), "UTC", "en"), null);
});

test("weekdays are named in the owner's language", () => {
  assert.equal(weekdayName(0, "en"), "Sunday");
  assert.equal(weekdayName(1, "tr"), "Pazartesi");
  assert.equal(weekdayName(7, "en"), "Sunday");
});
