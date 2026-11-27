import assert from "node:assert/strict";
import { test } from "node:test";
import { nameFromTask, parseAutomationSentence } from "./automationSentence.ts";

const TODAY = new Date(2026, 10, 27, 12, 0); // Friday 27 November 2026, local time

const when = (text: string) => {
  const r = parseAutomationSentence(text, TODAY);
  return r.simple
    ? { freq: r.simple.freq, time: r.simple.time, weekday: r.simple.weekday, monthDay: r.simple.monthDay, date: r.simple.date, task: r.task }
    : { freq: null, task: r.task };
};

test("the owner's example: every weekday at 9:00, then the task", () => {
  const r = parseAutomationSentence("Every weekday at 9:00, check my inbox and summarise", TODAY);
  assert.equal(r.simple?.freq, "weekdays");
  assert.equal(r.simple?.time, "09:00");
  assert.equal(r.whenText, "Every weekday at 9:00");
  assert.equal(r.task, "check my inbox and summarise");
});

test("English phrasings", () => {
  assert.deepEqual(when("Every day at 7am summarise the news"), { freq: "daily", time: "07:00", weekday: 1, monthDay: 1, date: "", task: "summarise the news" });
  assert.equal(when("Daily at 18:30: back up my notes").time, "18:30");
  assert.equal(when("every morning at 6.45, read my calendar").time, "06:45");
  assert.equal(when("at 9pm every day, tidy my downloads").time, "21:00");
  assert.deepEqual(when("Every Monday at 8:00 plan my week"), { freq: "weekly", time: "08:00", weekday: 1, monthDay: 1, date: "", task: "plan my week" });
  assert.equal(when("On Fridays at 17:00, send the weekly report").weekday, 5);
  assert.deepEqual(when("Every month on the 3rd at 10:00 pay reminder"), { freq: "monthly", time: "10:00", weekday: 1, monthDay: 3, date: "", task: "pay reminder" });
  assert.deepEqual(when("Tomorrow at 10:15 remind me to call Ann"), { freq: "once", time: "10:15", weekday: 1, monthDay: 1, date: "2026-11-28", task: "remind me to call Ann" });
  assert.equal(when("weekdays at 8 am and check the build").task, "check the build");
});

test("Turkish phrasings, including the dotted capital İ", () => {
  assert.deepEqual(when("Hafta içi her gün 9:00'da gelen kutumu kontrol et ve özetle"), { freq: "weekdays", time: "09:00", weekday: 1, monthDay: 1, date: "", task: "gelen kutumu kontrol et ve özetle" });
  assert.equal(when("HAFTA İÇİ saat 8'de rapor hazırla").freq, "weekdays");
  assert.deepEqual(when("Her gün saat 07.30'da haberleri özetle"), { freq: "daily", time: "07:30", weekday: 1, monthDay: 1, date: "", task: "haberleri özetle" });
  assert.equal(when("Her pazartesi 08:00'de haftamı planla").weekday, 1);
  assert.equal(when("Cumaları 17:00'de haftalık raporu gönder").weekday, 5);
  assert.equal(when("Her Salı 10:00'da toplantı notlarını topla").weekday, 2);
  assert.deepEqual(when("Her ayın 15'inde 10:00'da fatura hatırlat"), { freq: "monthly", time: "10:00", weekday: 1, monthDay: 15, date: "", task: "fatura hatırlat" });
  assert.equal(when("Yarın 14:00'te Ayşe'yi aramamı hatırlat").date, "2026-11-28");
});

test("no frequency means no schedule; the whole text stays the task", () => {
  assert.deepEqual(when("check my inbox and summarise"), { freq: null, task: "check my inbox and summarise" });
  // A time alone does not say how often.
  assert.equal(when("at 9, check my inbox").freq, null);
  // A bare weekday is a one-off date: not guessed.
  assert.equal(when("Monday at 9 plan the week").freq, null);
  assert.equal(when("Pazartesi 9'da haftayı planla").freq, null);
});

test("numbers that are not times stay in the task", () => {
  const r = parseAutomationSentence("Every day 15 sites check for news", TODAY);
  assert.equal(r.simple?.freq, "daily");
  assert.equal(r.simple?.time, "09:00");
  assert.equal(r.task, "15 sites check for news");
});

test("a second 'when' belongs to the task, and invalid clocks are not times", () => {
  assert.equal(when("Every day at 9:00 check what changed every day").task, "check what changed every day");
  assert.equal(when("Every day at 25:00 do it").time, "09:00");
});

test("a name comes from the first words of the task", () => {
  assert.equal(nameFromTask("check my inbox and summarise."), "Check my inbox and summarise");
  assert.equal(nameFromTask("ışıkları kontrol et", "tr"), "Işıkları kontrol et");
  const long = nameFromTask("summarise every unread message in my work inbox and draft replies for the urgent ones");
  assert.ok(long.length <= 49 && long.endsWith("…"), long);
  assert.equal(nameFromTask("   "), "");
});
