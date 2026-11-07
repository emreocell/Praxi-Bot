import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import type { RunState } from "../state/timeline.ts";
import {
  decideNotification,
  LONG_RUN_MS,
  type NotifyContext,
  notificationText
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
  assert.equal(open.body, "notify.detail.needsYou:Run rm -rf build");
  const priv = notificationText(d, "Atlas", true, t);
  assert.deepEqual(priv, { title: "Atlas", body: "notify.generic.needsYou" });
});
