import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { RunEntry, RunPhase, StopRecord } from "./runLedger.ts";
import {
  describeStop,
  isNewerNativeSnapshot,
  nextWidgetStop,
  reduceWidget,
  STOPPED_VISIBLE_MS,
  type StopStatus,
  stopDetail,
  stopTitleKey,
  WIDGET_CONFIRMED_MS,
  WIDGET_STOP_INITIAL,
  type WidgetView
} from "./stopStatus.ts";

function record(over: Partial<StopRecord> = {}): StopRecord {
  return {
    id: "s1",
    origin: "banner",
    kind: "all",
    nativeSeqs: [1],
    awaitingEcho: false,
    known: [],
    members: [],
    syncedAtDecision: true,
    decidedMono: 0,
    scope: "all_existing",
    phase: "in_flight",
    reached: false,
    firstSeen: null,
    audited: true,
    attempts: 1,
    retryIndex: 0,
    lastError: null,
    supersedes: null,
    successor: null,
    watchFromObs: null,
    session: "s",
    settledMono: null,
    ...over
  };
}

function run(id: string, phase: RunPhase, error: string | null = null): RunEntry {
  return { id, botId: "b", phase, stopId: "s1", quarantinedBy: null, seenObs: 0, processing: true, error };
}

const at = (records: StopRecord[], runs: RunEntry[], connected = true, now = 100) =>
  describeStop({ records, runs, now, connected });

// daemon outcome x native state -> what the user sees (the PR's state table)
test("state table: every daemon outcome maps to an honest status", () => {
  const two = ["a", "b"];
  const rows: [string, StopRecord[], RunEntry[], boolean, Partial<StopStatus> | null][] = [
    ["no client yet", [record({ members: two })], [run("a", "held"), run("b", "held")], false,
      { kind: "unreachable", tone: "warning", localHalted: true, dismissible: false, pendingSweep: true }],
    ["transport error", [record({ members: two, lastError: "network", phase: "retrying" })], [run("a", "held"), run("b", "held")], true,
      { kind: "unreachable" }],
    ["sent, no answer yet", [record({ members: two })], [run("a", "held"), run("b", "held")], true,
      { kind: "stopping", tone: "progress" }],
    ["cancel_requested", [record({ members: two, reached: true, phase: "retrying" })], [run("a", "cancel_requested"), run("b", "stopped")], true,
      { kind: "stopping", stopped: 1, stopping: 1 }],
    ["failed member", [record({ members: two, reached: true, phase: "retrying" })], [run("a", "failed", "busy"), run("b", "stopped")], true,
      { kind: "partial", tone: "danger", failed: 1, error: "busy" }],
    ["all terminal", [record({ members: two, reached: true, phase: "settled", settledMono: 90 })], [], true,
      { kind: "stopped", tone: "success", stopped: 2, total: 2 }],
    ["nothing was running", [record({ reached: true, phase: "settled", settledMono: 90 })], [], true,
      { kind: "stopped", total: 0 }],
    ["audit row failed", [record({ reached: true, phase: "settled", settledMono: 90, audited: false })], [], true,
      { kind: "stopped", audited: false }],
    ["expired, successor working", [
      record({ id: "s1", members: ["a"], phase: "expired", successor: "s2", settledMono: 50 }),
      record({ id: "s2", members: ["a"], supersedes: "s1", scope: "listed" })
    ], [run("a", "held")], true, { kind: "expired", tone: "warning", stopId: "s1", dismissible: false }],
    ["expired, nothing held", [record({ phase: "expired", settledMono: 90, reached: false })], [], true,
      { kind: "expired", dismissible: true }],
    ["restored, unconfirmable", [record({ phase: "unconfirmable" })], [], false,
      { kind: "unconfirmable", tone: "warning" }]
  ];
  for (const [name, records, runs, connected, expected] of rows) {
    const s = at(records, runs, connected);
    assert.ok(s, name);
    for (const [k, v] of Object.entries(expected ?? {})) {
      assert.deepEqual((s as unknown as Record<string, unknown>)[k], v, `${name}: ${k}`);
    }
  }
});

test("cancel_requested is never counted as stopped", () => {
  const s = at([record({ members: ["a"], reached: true, phase: "retrying" })], [run("a", "cancel_requested")]);
  assert.equal(s?.stopped, 0);
  assert.notEqual(s?.kind, "stopped");
});

test("a confirmed Stop disappears after a short while; a dismissed one at once", () => {
  const settled = [record({ reached: true, phase: "settled", settledMono: 0 })];
  assert.equal(at(settled, [], true, STOPPED_VISIBLE_MS)?.kind, "stopped");
  assert.equal(at(settled, [], true, STOPPED_VISIBLE_MS + 1), null);
  assert.equal(describeStop({ records: settled, runs: [], now: 1, connected: true, dismissed: new Set(["s1"]) }), null);
});

test("single-run cancels from the chat are not shown as a global Stop", () => {
  assert.equal(at([record({ kind: "single" })], []), null);
});

test("every title and detail key the status can produce exists in en and tr", () => {
  const read = (l: string) =>
    JSON.parse(readFileSync(new URL(`../i18n/locales/${l}.json`, import.meta.url), "utf8")) as Record<string, unknown>;
  const has = (obj: Record<string, unknown>, key: string, count: number) => {
    const get = (k: string) => k.split(".").reduce<unknown>((o, p) => (o as Record<string, unknown> | undefined)?.[p], obj);
    return typeof get(key) === "string" || typeof get(`${key}_${count === 1 ? "one" : "other"}`) === "string";
  };
  const kinds = ["stopping", "unreachable", "partial", "expired", "unconfirmed", "unconfirmable", "stopped"] as const;
  for (const locale of ["en", "tr"]) {
    const dict = read(locale);
    for (const kind of kinds) {
      for (const total of [0, 1, 3]) {
        for (const held of [0, 2]) {
          const status: StopStatus = {
            stopId: "x", kind, tone: "progress", total, stopped: 0, stopping: held, failed: 0,
            quarantined: 1, pendingSweep: held > 0, localHalted: true, audited: true,
            dismissible: total === 3, error: null
          };
          assert.ok(has(dict, stopTitleKey(status), total), `${locale}: ${stopTitleKey(status)}`);
          const d = stopDetail(status);
          assert.ok(has(dict, d.key, d.params.count ?? 0), `${locale}: ${d.key}`);
        }
      }
    }
    for (const key of ["stopStatus.notAudited", "stopStatus.dismiss", "stopStatus.progressLabel", "stopStatus.stopAllNow", "stopStatus.letContinue", "widget.stopPending", "widget.stopConfirmed", "widget.stopUnknown"]) {
      assert.ok(has(dict, key, 0), `${locale}: ${key}`);
    }
  }
});

test("widget: pending while stopSeq > stopAckedSeq, confirmed briefly only once acked", () => {
  let s = nextWidgetStop(WIDGET_STOP_INITIAL, { stopSeq: 0, stopAckedSeq: 0 }, 0);
  assert.equal(s.kind, "none");
  s = nextWidgetStop(s, { stopSeq: 1, stopAckedSeq: 0 }, 10);
  assert.equal(s.kind, "pending");
  assert.equal(s.pendingSeq, 1);
  s = nextWidgetStop(s, { stopSeq: 1, stopAckedSeq: 1 }, 20);
  assert.equal(s.kind, "confirmed");
  assert.equal(nextWidgetStop(s, null, 20 + WIDGET_CONFIRMED_MS).kind, "confirmed");
  assert.equal(nextWidgetStop(s, null, 21 + WIDGET_CONFIRMED_MS).kind, "none");
});

test("widget: a stale pre-Stop snapshot or a failed read never confirms", () => {
  const pending = nextWidgetStop(WIDGET_STOP_INITIAL, { stopSeq: 1, stopAckedSeq: 0 }, 0);
  // an older snapshot delivered late (stopSeq 0, ack 0)
  const stale = nextWidgetStop(pending, { stopSeq: 0, stopAckedSeq: 0 }, 5);
  assert.equal(stale.kind, "pending");
  assert.equal(stale.pendingSeq, 1);
  // getControlState() failed: the fallback snapshot has no counters
  const failed = nextWidgetStop(pending, {}, 6);
  assert.equal(failed.kind, "pending");
  assert.equal(failed.unknown, true, "shown as 'could not confirm'");
  assert.equal(nextWidgetStop(pending, null, 7).kind, "pending");
  // a fresh read clears "unknown"; only an ack covering seq 1 confirms
  const fresh = nextWidgetStop(failed, { stopSeq: 1, stopAckedSeq: 0 }, 8);
  assert.equal(fresh.unknown, false);
  assert.equal(nextWidgetStop(fresh, { stopSeq: 2, stopAckedSeq: 1 }, 9).kind, "pending", "ack 1 does not cover seq 2");
  // once confirmed, a late pre-ack snapshot does not bring "pending" back
  const confirmed = nextWidgetStop(fresh, { stopSeq: 1, stopAckedSeq: 1 }, 10);
  assert.equal(confirmed.kind, "confirmed");
  assert.equal(nextWidgetStop(confirmed, { stopSeq: 1, stopAckedSeq: 0 }, 11).kind, "confirmed");
});

test("widget: modes are applied only from newer revisions; a failed read never becomes Idle", () => {
  type Snap = { mode: string; stateRev?: number; stopSeq?: number; stopAckedSeq?: number };
  let v: WidgetView<Snap> = { stop: WIDGET_STOP_INITIAL, lastRev: -1, control: { mode: "idle" } };
  v = reduceWidget(v, { mode: "human_driving", stateRev: 5, stopSeq: 0, stopAckedSeq: 0 }, 0);
  v = reduceWidget(v, { mode: "bot_driving", stateRev: 4, stopSeq: 0, stopAckedSeq: 0 }, 1); // late
  assert.equal(v.control.mode, "human_driving");
  v = reduceWidget(v, null, 2);
  assert.equal(v.control.mode, "human_driving");
  assert.equal(isNewerNativeSnapshot(5, { stateRev: 6 }), false, "no counters: a failed read");
});
