import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import type { RunState } from "../state/timeline.ts";
import {
  decideNotification,
  LONG_RUN_MS,
  type NotifyContext,
  alwaysOffered,
  approvalActions,
  notificationText,
  resolveNotificationAction
} from "./notifyRules.ts";

const run: RunState = {
  id: "r1",
  chatId: "c1",
  botId: "b1",
  startedMs: 10_000,
  items: []
};

function env(
  event: DaemonEvent,
  ts_ms: number,
  parent?: string
): GlobalEnvelope {
  return { v: 1, seq: 1, gseq: 1, ts_ms, event, parent_run_id: parent };
}

const ctx = (over: Partial<NotifyContext> = {}): NotifyContext => ({
  run,
  appStartMs: 5_000,
  focused: false,
  prefs: { notifyNeedsYou: true, notifyLongRun: true, notifyFailed: true },
  ...over
});

const approval: DaemonEvent = {
  type: "approval_needed",
  run_id: "r1",
  call_id: "k1",
  tool_name: "shell",
  input: {},
  summary: "Run rm -rf build"
};

test("approval or question notifies only when unfocused", () => {
  const d = decideNotification(env(approval, 11_000), ctx());
  assert.equal(d?.kind, "needsYou");
  assert.equal(d?.chatId, "c1");
  assert.equal(
    decideNotification(env(approval, 11_000), ctx({ focused: true })),
    null
  );
  const q = decideNotification(
    env(
      {
        type: "question_asked",
        run_id: "r1",
        call_id: "k",
        question: "Which?",
        options: []
      },
      11_000
    ),
    ctx()
  );
  assert.equal(q?.detail, "Which?");
});

test("replayed history never notifies", () => {
  assert.equal(decideNotification(env(approval, 4_000), ctx()), null);
});

test("per-kind toggles", () => {
  const prefs = {
    notifyNeedsYou: false,
    notifyLongRun: false,
    notifyFailed: false
  };
  assert.equal(decideNotification(env(approval, 11_000), ctx({ prefs })), null);
  const done: DaemonEvent = { type: "run_finished", run_id: "r1", error: null };
  assert.equal(
    decideNotification(env(done, 10_000 + LONG_RUN_MS + 1), ctx({ prefs })),
    null
  );
  const failed: DaemonEvent = {
    type: "run_finished",
    run_id: "r1",
    error: "boom"
  };
  assert.equal(decideNotification(env(failed, 12_000), ctx({ prefs })), null);
});

test("long run finish: only over 30 s", () => {
  const done: DaemonEvent = { type: "run_finished", run_id: "r1", error: null };
  assert.equal(
    decideNotification(env(done, 10_000 + LONG_RUN_MS), ctx()),
    null
  );
  assert.equal(
    decideNotification(env(done, 10_000 + LONG_RUN_MS + 1), ctx())?.kind,
    "longRun"
  );
});

test("failed run notifies; a cancellation and a child run do not", () => {
  const failed: DaemonEvent = {
    type: "run_finished",
    run_id: "r1",
    error: "provider down"
  };
  assert.equal(decideNotification(env(failed, 12_000), ctx())?.kind, "failed");
  const cancelled: DaemonEvent = {
    type: "run_finished",
    run_id: "r1",
    error: "cancelled"
  };
  assert.equal(decideNotification(env(cancelled, 12_000), ctx()), null);
  assert.equal(decideNotification(env(failed, 12_000, "p1"), ctx()), null);
});

test("private mode keeps only bot name and generic text", () => {
  const t = (k: string, v?: Record<string, string>) =>
    v ? `${k}:${v.detail}` : k;
  const d = decideNotification(env(approval, 11_000), ctx());
  assert.ok(d);
  const open = notificationText(d, "Atlas", false, t);
  // an approval is worded from the tool, never from the raw summary
  assert.ok(open.body.startsWith("notify.approval.other"));
  assert.ok(!open.body.includes("rm -rf"));
  const question = notificationText({ ...d, approval: undefined }, "Atlas", false, t);
  assert.equal(question.body, "notify.detail.needsYou:Run rm -rf build");
  const priv = notificationText(d, "Atlas", true, t);
  assert.deepEqual(priv, { title: "Atlas", body: "notify.generic.needsYou" });
});

// ---- approval toast: human sentence and buttons

const tr = (k: string, v?: Record<string, string>) =>
  `${k}${v ? JSON.stringify(v) : ""}`;

const appr = (details: unknown = undefined, toolName = "mcp:playwright/browser_evaluate") => ({
  callId: "call-1",
  toolName,
  input: { function: "() => document.title" },
  details
});

test("approval toast says one human sentence, never the raw summary, json or ids", () => {
  const d = {
    kind: "needsYou" as const,
    runId: "r1",
    chatId: "c1",
    botId: "b1",
    detail: "pc:ca99609c mcp:playwright/browser_evaluate {\"function\":\"() =>\"}",
    approval: appr()
  };
  const text = notificationText(d, "Praxi", false, tr);
  assert.ok(text.body.startsWith("notify.approval.mcp"));
  assert.ok(text.body.includes('"app":"Playwright"'));
  assert.ok(text.body.includes('"action":"browser evaluate"'));
  assert.ok(!text.body.includes("function") && !text.body.includes("ca99609c") && !text.body.includes("pc:"));
  assert.deepEqual(text.actions?.map((a) => a.id), ["allow", "always", "deny"]);
  assert.equal(text.callId, "call-1");
  // private mode: no names, no buttons
  const priv = notificationText(d, "Praxi", true, tr);
  assert.equal(priv.actions, undefined);
  assert.equal(priv.body, "notify.generic.needsYou");
});

test("always-allow is offered only when the card would offer it; special cards get no buttons", () => {
  assert.equal(alwaysOffered(undefined), true);
  assert.equal(alwaysOffered({ once_only: true }), false);
  assert.equal(alwaysOffered({ why: "egress", once_only: false }), false);
  assert.deepEqual(approvalActions(appr({ once_only: true })), ["allow", "deny"]);
  assert.deepEqual(approvalActions(appr({ kind: "egress", once_only: false })), []);
  assert.deepEqual(approvalActions(appr(undefined, "gmail.send_email")), []);
});

test("a pressed button is re-checked against the pending approval and uses the card's scopes", () => {
  assert.deepEqual(resolveNotificationAction("allow", appr()), {
    kind: "decide", decision: "approve", scope: "once"
  });
  assert.deepEqual(resolveNotificationAction("always", appr()), {
    kind: "decide", decision: "approve", scope: "always"
  });
  assert.deepEqual(resolveNotificationAction("deny", appr()), { kind: "decide", decision: "deny" });
  // not offered now -> never approves, just opens the app
  assert.deepEqual(resolveNotificationAction("always", appr({ once_only: true })), { kind: "open" });
  assert.deepEqual(resolveNotificationAction("allow", undefined), { kind: "open" });
  assert.deepEqual(resolveNotificationAction("format-disk", appr()), { kind: "open" });
});

test("a scheduled job follows the notification choice; an approval always notifies", () => {
  const finished = (error: string | null): DaemonEvent =>
    ({ type: "run_finished", run_id: "r1", error }) as DaemonEvent;
  const pick = (e: DaemonEvent, c: "always" | "problems" | "never") =>
    decideNotification(env(e, 11_000), ctx({ scheduleNotify: c }));
  // always: a quiet success is announced too
  assert.equal(pick(finished(null), "always")?.kind, "scheduleDone");
  // only problems: success is silent, a failure still notifies
  assert.equal(pick(finished(null), "problems"), null);
  assert.equal(pick(finished("boom"), "problems")?.kind, "failed");
  // never: nothing at the end of the run, but a pending approval is never hidden
  assert.equal(pick(finished("boom"), "never"), null);
  assert.equal(pick(approval, "never")?.kind, "needsYou");
});
