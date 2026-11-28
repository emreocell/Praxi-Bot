import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
const view = read("../views/SchedulesView.tsx");
const form = view.slice(view.indexOf("function ScheduleForm("));
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const lookup = (tree: Record<string, unknown>, key: string): unknown =>
  key.split(".").reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], tree);

test("the schedule editor is one flow: sentence, example chips, when, bot and name, what to do, then the folded options", () => {
  const order = [
    'data-testid="schedule-sentence"',
    'data-testid="schedule-examples"',
    "data-testid={`schedule-freq-${f}`}",
    'data-testid="schedule-when-summary"',
    'data-testid="schedule-bot"',
    'data-testid="schedule-name"',
    "schedule-task-kind-${kind",
    'data-testid="schedule-advanced"'
  ].map((id) => form.indexOf(id));
  assert.ok(order.every((at) => at > 0), String(order));
  assert.deepEqual([...order].sort((a, b) => a - b), order);
});

test("the examples are small chips, not a row of big cards", () => {
  assert.doesNotMatch(form, /styles\.template\b|styles\.templateRow/);
  assert.match(form, /className=\{styles\.exampleChip\}[\s\S]{0,80}title=\{x\.blurb\}/);
  assert.doesNotMatch(read("../views/SchedulesView.module.css"), /\.templateRow|\.templateBlurb/);
});

test("the notify choice, time zone, cron and missed-run rules live in the folded section", () => {
  const fold = form.indexOf('data-testid="schedule-advanced"');
  for (const part of [
    "data-testid={`schedule-notify-${c}`}",
    't("schedules.form.tz")',
    "schedules.form.kind.cron",
    "schedules.form.misfire\"",
    "schedules.enabled"
  ]) {
    assert.ok(form.indexOf(part) > fold, part);
  }
  // The fold says what it holds, and "Change time zone" next to the summary opens it.
  assert.match(form, /scheduleEditor\.moreLine/);
  assert.match(form, /const openZone = \(\) => \{\s*setMoreOpen\(true\);/);
});

test("same backend: the form still saves through createSchedule / updateSchedule with buildInput", () => {
  assert.match(form, /const input = buildInput\(draft, spec, editing\);/);
  assert.match(form, /client\.updateSchedule\(editing\.id, input\)/);
  assert.match(form, /client\.createSchedule\(input\)/);
});

test("every editor text exists in English and Turkish", () => {
  const keys = new Set([...form.matchAll(/t\("(scheduleEditor\.[a-zA-Z.]+)"/g)].map((m) => m[1]));
  for (const k of form.matchAll(/t\("(schedules\.form\.[a-zA-Z.]+)"/g)) keys.add(k[1]);
  for (const k of ["daily", "weekdays", "weekly", "monthly", "once", "onceNoDate"]) keys.add(`scheduleEditor.summary.${k}`);
  assert.ok(keys.size >= 10);
  for (const lang of ["en", "tr"]) {
    for (const key of keys) assert.equal(typeof lookup(locale(lang), key), "string", `${lang}: ${key}`);
  }
});
