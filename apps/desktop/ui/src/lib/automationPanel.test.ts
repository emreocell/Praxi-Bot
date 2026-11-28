import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
const panel = read("../components/BotPanelAutomation.tsx");
const schedules = read("../views/SchedulesView.tsx");
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const lookup = (tree: Record<string, unknown>, key: string): unknown =>
  key.split(".").reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], tree);

test("the Automation tab: one sentence to add, one list, the event kinds, then missions", () => {
  const order = ["automation-empty", "automation-add-", "automation-file", "automation-missions", "bot-panel-schedules"].map((id) =>
    panel.indexOf(`data-testid${id.endsWith("-") ? "={`" : '="'}${id}`)
  );
  order.unshift(panel.indexOf("<QuickCreate "));
  assert.ok(order.every((at) => at > 0), String(order));
  assert.deepEqual([...order].sort((a, b) => a - b), order);
  // Every event kind but the file watcher opens the builder; the file kind explains (the bot sets it up in chat).
  assert.match(panel, /const EVENT_CARDS = \["mail", "webhook", "feed", "page", "calendar", "home_assistant"\] as const/);
  assert.match(panel, /EVENT_CARDS\.map/);
});

test("every Automation text exists in English and Turkish", () => {
  const keys = new Set([...panel.matchAll(/t\("(automation\.[a-zA-Z.]+)"/g), ...schedules.matchAll(/t\("(automation\.[a-zA-Z.]+)"/g)].map((m) => m[1]));
  for (const kind of ["mail", "webhook", "file", "feed", "page", "calendar", "home_assistant"]) for (const part of ["title", "line"]) keys.add(`automation.events.${kind}.${part}`);
  for (const k of ["mail", "webhook", "time", "feed", "page", "calendar", "home_assistant"]) keys.add(`automation.builder.title.${k}`);
  for (const k of ["feed", "page", "calendar", "home_assistant"]) keys.add(`automation.what.${k}`);
  for (const k of ["today", "tomorrow", "yesterday"]) keys.add(`automation.day.${k}`);
  for (const k of ["ok", "failed", "missed", "running"]) keys.add(`automation.last.${k}`);
  for (const k of ["failed", "missed"]) keys.add(`automation.state.${k}`);
  for (const lang of ["en", "tr"]) {
    const tree = locale(lang);
    for (const key of keys) {
      const value = lookup(tree, key) ?? lookup(tree, `${key}_other`);
      assert.equal(typeof value, "string", `${lang}: ${key}`);
    }
  }
});

test("a schedule row has one primary button, and history folds", () => {
  const row = schedules.slice(schedules.indexOf("function ScheduleRow("), schedules.indexOf("function ScheduleCalendar("));
  assert.equal([...row.matchAll(/variant="secondary"/g)].length, 3, "reclaim / retry / run now: exactly one is shown");
  assert.match(row, /primary\.kind === "reclaim" \?[\s\S]*primary\.kind === "retry" \?/);
  assert.match(row, /aria-expanded=\{expanded\}/);
  assert.match(row, /\{expanded \? \(/);
  // Deleting moved into the edit form, behind one confirmation.
  assert.doesNotMatch(row, /deleteSchedule/);
  assert.match(schedules, /schedule-delete-confirm/);
});
