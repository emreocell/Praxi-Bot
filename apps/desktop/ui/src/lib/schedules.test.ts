import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiClient } from "../api/client.ts";
import type { Schedule } from "../api/types.ts";
import { applyEvent, EMPTY_EVENTS } from "../state/timeline.ts";
import {
  affectsSchedules,
  buildInput,
  draftFromSchedule,
  emptyDraft,
  intervalParts,
  parsePayload,
  parseSpec,
  runOutcome,
  runsOfSchedule,
  scheduleStatus,
  sortSchedules,
  specLabel,
  toggleInput,
  validateDraft
} from "./schedules.ts";

function sched(over: Partial<Schedule> = {}): Schedule {
  return {
    id: "s1",
    bot_id: "b1",
    name: "Morning",
    enabled: true,
    has_webhook: false,
    created_at_ms: 1,
    misfire_policy: "skip",
    next_run_at_ms: 1000,
    spec: { cron: "0 9 * * 1-5 Europe/Istanbul" },
    payload: { kind: "agent_prompt", text: "hi" },
    ...over
  };
}

test("parseSpec reads the three shapes and rejects junk", () => {
  assert.deepEqual(parseSpec({ cron: "* * * * *" }), { cron: "* * * * *" });
  assert.deepEqual(parseSpec({ every: { seconds: 60 } }), {
    every: { seconds: 60 }
  });
  assert.deepEqual(parseSpec({ at: { unix_ms: 5 } }), { at: { unix_ms: 5 } });
  assert.equal(parseSpec({ other: 1 }), null);
  assert.equal(parseSpec(null), null);
});

test("parsePayload", () => {
  assert.deepEqual(parsePayload({ kind: "agent_prompt", text: "x" }), {
    kind: "agent_prompt",
    text: "x"
  });
  assert.equal(parsePayload({ kind: "workflow", name: "w" }).kind, "workflow");
  assert.equal(parsePayload({ kind: "tool", name: "t" }).kind, "tool");
  assert.equal(parsePayload({ kind: "nope" }).kind, "unknown");
});

test("intervalParts picks the largest exact unit", () => {
  assert.deepEqual(intervalParts(86400 * 2), { unit: "days", value: 2 });
  assert.deepEqual(intervalParts(7200), { unit: "hours", value: 2 });
  assert.deepEqual(intervalParts(600), { unit: "minutes", value: 10 });
  assert.deepEqual(intervalParts(90), { unit: "seconds", value: 90 });
});

test("specLabel", () => {
  const fmt = (ms: number) => `t${ms}`;
  assert.equal(specLabel({ cron: "a" }, fmt).i18nKey, "schedules.spec.cron");
  assert.deepEqual(specLabel({ every: { seconds: 600 } }, fmt), {
    i18nKey: "schedules.spec.every.minutes",
    values: { count: 10 }
  });
  assert.deepEqual(specLabel({ at: { unix_ms: 7 } }, fmt).values, {
    when: "t7"
  });
  assert.equal(specLabel(1, fmt).i18nKey, "schedules.spec.unknown");
});

test("scheduleStatus and sorting", () => {
  assert.equal(scheduleStatus(sched({ enabled: false })), "disabled");
  assert.equal(scheduleStatus(sched({ next_run_at_ms: null })), "finished");
  assert.equal(scheduleStatus(sched()), "enabled");
  const sorted = sortSchedules([
    sched({ id: "off", name: "A", enabled: false }),
    sched({ id: "late", name: "B", next_run_at_ms: 9000 }),
    sched({ id: "soon", name: "C", next_run_at_ms: 10 })
  ]);
  assert.deepEqual(
    sorted.map((s) => s.id),
    ["soon", "late", "off"]
  );
});

test("run_started carries the schedule id into the run state", () => {
  const ev = (
    gseq: number,
    event: Parameters<typeof applyEvent>[1]["event"]
  ) => ({ v: 1, seq: gseq, ts_ms: gseq * 100, gseq, event });
  let s = applyEvent(
    EMPTY_EVENTS,
    ev(1, {
      type: "run_started",
      run_id: "r1",
      chat_id: "c",
      bot_id: "b",
      schedule_id: "s1"
    })
  );
  s = applyEvent(
    s,
    ev(2, {
      type: "run_started",
      run_id: "r2",
      chat_id: "c",
      bot_id: "b",
      schedule_id: "s1"
    })
  );
  s = applyEvent(
    s,
    ev(3, { type: "run_started", run_id: "r3", chat_id: "c", bot_id: "b" })
  );
  s = applyEvent(s, ev(4, { type: "run_finished", run_id: "r1", error: "boom" }));
  const runs = runsOfSchedule(s.runs, "s1");
  assert.deepEqual(
    runs.map((r) => r.id),
    ["r2", "r1"]
  );
  assert.equal(runOutcome(runs[0]), "running");
  assert.equal(runOutcome(runs[1]), "failed");
  assert.equal(runsOfSchedule(s.runs, "s1", 1).length, 1);
});

test("affectsSchedules", () => {
  assert.equal(
    affectsSchedules({ type: "run_started", schedule_id: "s" }),
    true
  );
  assert.equal(affectsSchedules({ type: "run_started" }), false);
  assert.equal(affectsSchedules({ type: "run_finished" }), true);
  assert.equal(affectsSchedules({ type: "message_delta" }), false);
});

test("validateDraft", () => {
  const d = emptyDraft("b1");
  assert.equal(validateDraft(d), "name");
  d.name = "n";
  d.botId = "";
  assert.equal(validateDraft(d), "bot");
  d.botId = "b1";
  assert.equal(validateDraft(d), "text");
  d.text = "every day at 9";
  assert.equal(validateDraft(d), "prompt");
  d.prompt = "do it";
  assert.equal(validateDraft(d), null);
  d.kind = "cron";
  assert.equal(validateDraft(d), "cron");
  // a workflow payload needs no prompt when editing
  const wf = sched({ payload: { kind: "workflow", name: "w" } });
  assert.equal(validateDraft(draftFromSchedule(wf), wf), null);
});

test("buildInput for natural, cron and keep", () => {
  const d = { ...emptyDraft("b1"), name: " N ", text: "daily", prompt: " go " };
  const parsed = { cron: "0 9 * * *" };
  const a = buildInput(d, parsed);
  assert.equal(a.name, "N");
  assert.deepEqual(a.spec, parsed);
  assert.deepEqual(a.payload, { kind: "agent_prompt", text: "go" });
  assert.equal(a.enabled, true);
  assert.throws(() => buildInput(d, null));

  const c = buildInput({ ...d, kind: "cron", cron: " * * * * * " }, null);
  assert.deepEqual(c.spec, { cron: "* * * * *" });

  const existing = sched({ spec: { every: { seconds: 60 } }, chat_id: "chat" });
  const draft = draftFromSchedule(existing);
  assert.equal(draft.kind, "keep");
  const k = buildInput(draft, null, existing);
  assert.deepEqual(k.spec, { every: { seconds: 60 } });
  assert.equal(k.chat_id, "chat");

  const wf = sched({ payload: { kind: "workflow", name: "w" } });
  assert.deepEqual(
    buildInput(draftFromSchedule(wf), null, wf).payload,
    wf.payload
  );
});

test("toggleInput flips enabled and keeps the rest", () => {
  const t = toggleInput(sched());
  assert.equal(t.enabled, false);
  assert.equal(t.name, "Morning");
  assert.deepEqual(t.spec, sched().spec);
});

test("ApiClient schedule routes hit the right URLs", async () => {
  const calls: { method: string; url: string; body?: string }[] = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async (url: URL, init: RequestInit) => {
    calls.push({
      method: init.method ?? "GET",
      url: String(url),
      body: init.body as string | undefined
    });
    const noBody = init.method === "DELETE";
    return new Response(noBody ? null : "{}", { status: noBody ? 204 : 200 });
  }) as typeof fetch;
  try {
    const c = new ApiClient({ base_url: "http://x/", token: "t" });
    const input = { bot_id: "b", name: "n", spec: {}, payload: {} };
    await c.listSchedules();
    await c.getSchedule("s1");
    await c.createSchedule(input);
    await c.updateSchedule("s1", input);
    await c.deleteSchedule("s1");
    await c.runScheduleNow("s1");
    await c.parseSchedule("her gun 09:00", "Europe/Istanbul");
  } finally {
    globalThis.fetch = realFetch;
  }
  assert.deepEqual(
    calls.map((c) => `${c.method} ${new URL(c.url).pathname}`),
    [
      "GET /v1/schedules",
      "GET /v1/schedules/s1",
      "POST /v1/schedules",
      "PUT /v1/schedules/s1",
      "DELETE /v1/schedules/s1",
      "POST /v1/schedules/s1/run-now",
      "POST /v1/schedules/parse"
    ]
  );
  assert.deepEqual(JSON.parse(calls[6].body ?? "{}"), {
    text: "her gun 09:00",
    tz: "Europe/Istanbul"
  });
});
