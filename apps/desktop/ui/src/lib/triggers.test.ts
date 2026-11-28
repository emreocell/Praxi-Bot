import assert from "node:assert/strict";
import test from "node:test";
import { EVENT_KINDS, WEB_MIN_POLL_SECONDS, clockTime, dailyTime, minutes, newSource, newTrigger, sampleEvent, triggerInput, triggerSpec } from "./triggers.ts";
import type { ScheduleSpec } from "./schedules.ts";

test("rules start off with bounded polling and visible safe defaults", () => {
  const rule = newTrigger();
  assert.equal(rule.enabled, false);
  assert.equal(rule.config.source.kind === "mail" && rule.config.source.poll_seconds, 120);
  assert.deepEqual(rule.config.limits, { per_hour: 10, per_day: 50 });
  const edited = triggerInput({ ...rule, id: "runtime-id", origin: "rule", count_today: 8 } as typeof rule);
  assert.equal("id" in edited, false);
  assert.equal("origin" in edited, false);
  edited.config.limits.per_hour = 2;
  assert.equal(rule.config.limits.per_hour, 10);
  assert.equal(clockTime(minutes("22:05")), "22:05");
});

test("saved intervals, one-time and weekday rules never masquerade as daily rules", () => {
  const specs: ScheduleSpec[] = [
    { every: { seconds: 3600 } },
    { at: { unix_ms: 1_800_000_000_000 } },
    { cron: "0 9 * * 1-5 Europe/Berlin" }
  ];
  for (const spec of specs) {
    assert.equal(dailyTime({ kind: "time", spec }), null);
    assert.deepEqual(triggerSpec("cron" in spec ? spec.cron : JSON.stringify(spec)), spec);
  }
  assert.deepEqual(dailyTime({ kind: "time", spec: { cron: "5 9 * * * Europe/Berlin" } }),
    { time: "09:05", zone: "Europe/Berlin" });
});

test("every event kind starts from the daemon's default pace and web addresses are never polled faster than every 15 minutes", () => {
  assert.deepEqual([...EVENT_KINDS], ["mail", "webhook", "feed", "page", "calendar", "home_assistant"]);
  for (const kind of EVENT_KINDS) assert.equal(newSource(kind).kind, kind);
  const web = [newSource("feed"), newSource("page")];
  for (const source of web) {
    assert.ok("poll_seconds" in source && source.poll_seconds >= WEB_MIN_POLL_SECONDS);
  }
  assert.deepEqual(newSource("calendar"), { kind: "calendar", account: null, minutes_before: 10, poll_seconds: 120 });
  assert.deepEqual(newSource("home_assistant"), { kind: "home_assistant", entity_id: "", to_state: null, poll_seconds: 60 });
  // Each call returns a new object: editing one draft cannot change the next.
  assert.notEqual(newSource("feed"), newSource("feed"));
});

test("a sample for the no-model preview looks like what the daemon hands the bot", () => {
  const page = sampleEvent({ kind: "page", url: "https://e.test/p", selector: "#price", poll_seconds: 3600 }, "$9");
  assert.deepEqual(page, { url: "https://e.test/p", selector: "#price", text: "$9", received_at: 0 });
  const door = sampleEvent({ kind: "home_assistant", entity_id: "binary_sensor.door", to_state: "on", poll_seconds: 60 }, "");
  assert.equal(door.state, "on");
  assert.equal(door.previous_state, "off");
  assert.equal(sampleEvent({ kind: "calendar", account: null, minutes_before: 15, poll_seconds: 120 }, "Dentist").summary, "Dentist");
  assert.equal(sampleEvent({ kind: "feed", url: "https://e.test/f", poll_seconds: 1800 }, "").title, "A new post");
});
