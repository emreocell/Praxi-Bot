import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  ControlModeName,
  ControlVerdict,
  DaemonEvent,
  GlobalEnvelope,
  Run
} from "../api/types.ts";
import {
  type BotMeta,
  ControlBridge,
  type Rect,
  refusalReason
} from "./controlBridge.ts";

const NOW = 1_000_000;

interface Calls {
  put: ControlModeName[];
  release: boolean[];
  decide: { id: string; decision: ControlVerdict; reason?: string }[];
  cancel: string[];
  begin: { rect: Rect | null; label: string; action: string }[];
  start: { bot: BotMeta; step: string }[];
  stop: number;
  clicks: { x: number; y: number }[];
}

function setup(beginResult: boolean | (() => boolean) = true) {
  const calls: Calls = {
    put: [],
    release: [],
    decide: [],
    cancel: [],
    begin: [],
    start: [],
    stop: 0,
    clicks: []
  };
  let now = NOW;
  let listRuns: () => Promise<Pick<Run, "id" | "bot_id" | "state">[]> = async () => [];
  const bridge = new ControlBridge({
    client: {
      listRuns: () => listRuns(),
      putControlState: async (m, release) => {
        calls.put.push(m);
        calls.release.push(release === true);
      },
      decideControl: async (id, decision, reason) =>
        void calls.decide.push({ id, decision, reason }),
      cancelRun: async (id) => void calls.cancel.push(id)
    },
    native: {
      beginAction: async (rect, label, action) => {
        calls.begin.push({ rect, label, action });
        return typeof beginResult === "function" ? beginResult() : beginResult;
      },
      startRun: async (bot, step) => void calls.start.push({ bot, step }),
      stopRun: async () => ++calls.stop,
      click: async (x, y) => void calls.clicks.push({ x, y })
    },
    botMeta: (id) => ({ botId: id, botName: `Bot ${id}` }),
    startingStep: "Working…",
    now: () => now
  });
  let gseq = 0;
  const feed = (event: DaemonEvent, ts = now) => {
    gseq += 1;
    const env: GlobalEnvelope = { v: 1, seq: gseq, ts_ms: ts, gseq, event };
    return bridge.handleEvent(env);
  };
  return {
    bridge,
    calls,
    feed,
    snapshot: (read: typeof listRuns) => { listRuns = read; },
    advance: (ms: number) => {
      now += ms;
    }
  };
}

const request = (id: string, rect: Rect | null = null, action = "click") =>
  ({
    type: "control_requested",
    request_id: id,
    run_id: "run-1",
    bot_id: "b1",
    rect,
    label: "Save",
    action
  }) satisfies DaemonEvent;

test("a started run makes the bot drive; the last finished run ends it", async () => {
  const t = setup();
  await t.feed({
    type: "run_started",
    run_id: "run-1",
    chat_id: "c",
    bot_id: "b1"
  });
  assert.equal(t.calls.start.length, 1);
  assert.equal(t.calls.start[0].bot.botName, "Bot b1");
  assert.equal(t.calls.start[0].step, "Working…");
  assert.equal(t.bridge.mode, "bot_driving");
  // a second run does not restart it
  await t.feed({
    type: "run_started",
    run_id: "run-2",
    chat_id: "c",
    bot_id: "b1"
  });
  assert.equal(t.calls.start.length, 1);
  await t.feed({ type: "run_finished", run_id: "run-1", error: null });
  assert.equal(t.calls.stop, 0, "run-2 is still live");
  await t.feed({ type: "run_finished", run_id: "run-2", error: null });
  assert.equal(t.calls.stop, 1);
  assert.equal(t.bridge.mode, "idle");
  assert.deepEqual(t.bridge.liveRuns(), []);
});

test("events from before the bridge existed (history replay) are ignored", async () => {
  const t = setup();
  await t.feed(
    { type: "run_started", run_id: "old", chat_id: "c", bot_id: "b1" },
    NOW - 60_000
  );
  assert.equal(t.calls.start.length, 0);
  assert.deepEqual(t.bridge.liveRuns(), []);
});

test("control_requested shows the target, posts proceed and ripples a click", async () => {
  const t = setup(true);
  const rect = { x: 100, y: 200, width: 40, height: 20 };
  await t.feed(request("r1", rect));
  assert.deepEqual(t.calls.begin, [{ rect, label: "Save", action: "click" }]);
  assert.deepEqual(t.calls.decide, [
    { id: "r1", decision: "proceed", reason: undefined }
  ]);
  assert.deepEqual(t.calls.clicks, [{ x: 120, y: 210 }]);
  // The request arrived while idle: the bridge started the run state first.
  assert.equal(t.calls.start.length, 1);
});

test("a refusal from the state machine is posted with a reason for the model", async () => {
  const t = setup(false);
  await t.bridge.onNativeState("human_driving");
  await t.feed(request("r2", null, "key"));
  assert.equal(t.calls.decide.length, 1);
  assert.equal(t.calls.decide[0].decision, "refuse");
  assert.match(t.calls.decide[0].reason ?? "", /taken control/);
  assert.equal(t.calls.begin[0].rect, null);
  assert.equal(t.calls.clicks.length, 0);
  assert.equal(t.calls.start.length, 0, "the human holds the wheel: no restart");
});

test("a request is answered once, and a stale one not at all", async () => {
  const t = setup();
  await t.feed(request("r3"));
  await t.feed(request("r3"));
  assert.equal(t.calls.decide.length, 1);
  t.advance(60_000);
  await t.feed(request("r4"), NOW + 1000);
  assert.equal(t.calls.decide.length, 1, "the daemon gave up on r4 long ago");
});

test("every native state change is reported to the daemon, and so is the heartbeat", async () => {
  const t = setup();
  await t.bridge.onNativeState("bot_driving");
  await t.bridge.onNativeState("human_driving");
  await t.bridge.heartbeat();
  assert.deepEqual(t.calls.put, [
    "bot_driving",
    "human_driving",
    "human_driving"
  ]);
});

test("only the user's explicit resume sends release:true, never a heartbeat or reopen", async () => {
  const t = setup();
  // Reopened UI: starts idle and heartbeats.
  await t.bridge.heartbeat();
  await t.bridge.onNativeState("human_driving");
  await t.bridge.heartbeat();
  await t.bridge.heartbeat();
  assert.deepEqual(t.calls.release, [false, false, false, false]);
  // The user hands the wheel back ("Bitirdim"): native state leaves human_driving.
  await t.bridge.onNativeState("idle");
  assert.equal(t.calls.release.at(-1), true);
  await t.bridge.heartbeat();
  assert.equal(t.calls.release.at(-1), false, "later heartbeats do not release");
  // Pause then resume to bot_driving.
  await t.bridge.onNativeState("paused");
  assert.equal(t.calls.release.at(-1), false);
  await t.bridge.onNativeState("bot_driving");
  assert.equal(t.calls.release.at(-1), true);
});

test("Stop (state goes idle while runs are live) cancels those runs", async () => {
  const t = setup();
  await t.feed({
    type: "run_started",
    run_id: "run-1",
    chat_id: "c",
    bot_id: "b1"
  });
  await t.feed({
    type: "run_started",
    run_id: "run-2",
    chat_id: "c",
    bot_id: "b1"
  });
  await t.bridge.onNativeState("bot_driving");
  await t.bridge.onNativeState("idle");
  assert.deepEqual(t.calls.cancel.sort(), ["run-1", "run-2"]);
  assert.deepEqual(t.bridge.liveRuns(), []);
  assert.equal(t.calls.put.at(-1), "idle");
  // idle again with nothing live cancels nothing more
  await t.bridge.onNativeState("idle");
  assert.equal(t.calls.cancel.length, 2);
});

test("a normal finish is not mistaken for Stop", async () => {
  const t = setup();
  await t.feed({
    type: "run_started",
    run_id: "run-1",
    chat_id: "c",
    bot_id: "b1"
  });
  await t.bridge.onNativeState("bot_driving");
  await t.feed({ type: "run_finished", run_id: "run-1", error: null });
  await t.bridge.onNativeState("idle"); // the event stopRun caused
  assert.deepEqual(t.calls.cancel, []);
});

test("refusal reasons name the state", () => {
  assert.match(refusalReason("human_driving"), /taken control/);
  assert.match(refusalReason("paused"), /paused/);
  assert.match(refusalReason("bot_driving"), /stopped/);
});

test("startRun preserves explicit botShape even when it differs from ID hash fallback", async () => {
  const calls: { bot: BotMeta; step: string }[] = [];
  const bridge = new ControlBridge({
    client: {
      listRuns: async () => [],
      putControlState: async () => undefined,
      decideControl: async () => undefined,
      cancelRun: async () => undefined
    },
    native: {
      beginAction: async () => true,
      startRun: async (bot, step) => void calls.push({ bot, step }),
      stopRun: async () => 1,
      click: async () => undefined
    },
    botMeta: (id) => ({
      botId: id,
      botName: "HeartBot",
      botShape: "heart",
      botColor: "#4c8dff"
    }),
    startingStep: "Working…",
    now: () => NOW
  });
  await bridge.handleEvent({
    v: 1,
    seq: 1,
    ts_ms: NOW,
    gseq: 1,
    event: {
      type: "run_started",
      run_id: "run-shape",
      chat_id: "c",
      bot_id: "bot-cube-0"
    }
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].bot.botId, "bot-cube-0");
  assert.equal(calls[0].bot.botShape, "heart");
});

test("reopening the app restores existing daemon runs so Stop cancels them", async () => {
  const t = setup();
  t.snapshot(async () => [{ id: "existing", bot_id: "b1", state: "running" }]);
  await t.feed({ type: "run_started", run_id: "existing", chat_id: "c", bot_id: "b1" }, NOW - 60_000);
  await t.bridge.restoreRuns();
  assert.deepEqual(t.bridge.liveRuns(), ["existing"]);
  assert.equal(t.calls.start.length, 1);
  await t.bridge.onNativeState("idle");
  assert.deepEqual(t.calls.cancel, ["existing"]);
});

test("restoration uses actual daemon states and does not release an existing human hold", async () => {
  const t = setup();
  await t.bridge.onNativeState("human_driving");
  t.snapshot(async () => [
    { id: "queued", bot_id: "b1", state: "queued" },
    { id: "waiting", bot_id: "b1", state: "waiting_approval" },
    { id: "paused", bot_id: "b1", state: "paused" },
    { id: "done", bot_id: "b1", state: "succeeded" },
    { id: "failed", bot_id: "b1", state: "failed" },
    { id: "cancelled", bot_id: "b1", state: "cancelled" }
  ]);
  await t.bridge.restoreRuns();
  assert.deepEqual(t.bridge.liveRuns().sort(), ["paused", "queued", "waiting"]);
  assert.equal(t.calls.start.length, 0);
  assert.equal(t.bridge.mode, "human_driving");
  assert.ok(t.calls.release.every((release) => !release));
});

test("a terminal event arriving during the initial snapshot wins over its stale row", async () => {
  const t = setup();
  let resolve!: (runs: Pick<Run, "id" | "bot_id" | "state">[]) => void;
  t.snapshot(() => new Promise((done) => { resolve = done; }));
  const restoring = t.bridge.restoreRuns();
  await t.feed({ type: "run_finished", run_id: "finished-during-read", error: null });
  resolve?.([{ id: "finished-during-read", bot_id: "b1", state: "running" }]);
  await restoring;
  assert.deepEqual(t.bridge.liveRuns(), []);
  assert.equal(t.calls.start.length, 0);
});

test("fresh starts received while the snapshot is pending stay live even when absent from it", async () => {
  const t = setup();
  let resolve!: (runs: Pick<Run, "id" | "bot_id" | "state">[]) => void;
  t.snapshot(() => new Promise((done) => { resolve = done; }));
  const restoring = t.bridge.restoreRuns();
  await t.feed({ type: "run_started", run_id: "new", bot_id: "b1", chat_id: "c" });
  resolve?.([]);
  await restoring;
  assert.deepEqual(t.bridge.liveRuns(), ["new"]);
  await t.bridge.onNativeState("idle");
  assert.deepEqual(t.calls.cancel, ["new"]);
});

test("Stop during restoration cancels snapshot runs without starting the native widget again", async () => {
  const t = setup();
  let resolve!: (runs: Pick<Run, "id" | "bot_id" | "state">[]) => void;
  t.snapshot(() => new Promise((done) => { resolve = done; }));
  const restoring = t.bridge.restoreRuns();
  await t.feed({ type: "run_started", run_id: "new", bot_id: "b1", chat_id: "c" });
  await t.bridge.onNativeState("idle");
  resolve?.([{ id: "existing", bot_id: "b1", state: "running" }]);
  await restoring;
  assert.deepEqual(t.calls.cancel.sort(), ["existing", "new"]);
  assert.deepEqual(t.bridge.liveRuns(), []);
  assert.equal(t.calls.start.length, 1);
});

test("disposing the old connection drops its active runs and prevents late cancellation", async () => {
  const old = setup();
  await old.feed({ type: "run_started", run_id: "old-daemon", bot_id: "b1", chat_id: "c" });
  old.bridge.dispose();
  const current = setup();
  current.snapshot(async () => [{ id: "current-daemon", bot_id: "b1", state: "running" }]);
  await current.bridge.restoreRuns();
  await old.bridge.onNativeState("idle");
  await current.bridge.onNativeState("idle");
  assert.deepEqual(old.bridge.liveRuns(), []);
  assert.deepEqual(old.calls.cancel, []);
  assert.deepEqual(current.calls.cancel, ["current-daemon"]);
});

test("a delayed snapshot cannot revive an already disposed daemon connection", async () => {
  const old = setup();
  let resolve!: (runs: Pick<Run, "id" | "bot_id" | "state">[]) => void;
  old.snapshot(() => new Promise((done) => { resolve = done; }));
  const restoring = old.bridge.restoreRuns();
  old.bridge.dispose();
  resolve?.([{ id: "old-daemon", bot_id: "b1", state: "running" }]);
  await restoring;
  assert.deepEqual(old.bridge.liveRuns(), []);
  assert.equal(old.calls.start.length, 0);
  assert.deepEqual(old.calls.cancel, []);
});

test("a failed initial run snapshot can be retried", async () => {
  const t = setup();
  t.snapshot(async () => { throw new Error("temporarily offline"); });
  await t.bridge.restoreRuns();
  t.snapshot(async () => [{ id: "existing", bot_id: "b1", state: "running" }]);
  await t.bridge.restoreRuns();
  assert.deepEqual(t.bridge.liveRuns(), ["existing"]);
});

test("a stale initial idle snapshot does not stop a freshly started run", async () => {
  const t = setup();
  await t.feed({ type: "run_started", run_id: "new", chat_id: "c", bot_id: "b1" });
  await t.bridge.initializeNativeState("idle");
  assert.equal(t.bridge.mode, "bot_driving");
  assert.deepEqual(t.bridge.liveRuns(), ["new"]);
  assert.deepEqual(t.calls.cancel, []);
  assert.ok(t.calls.release.every((release) => !release));
});

test("restoring only paused runs does not make the bot start processing", async () => {
  const t = setup();
  t.snapshot(async () => [{ id: "paused", bot_id: "b1", state: "paused" }]);
  await t.bridge.restoreRuns();
  assert.deepEqual(t.bridge.liveRuns(), ["paused"]);
  assert.equal(t.bridge.mode, "idle");
  assert.equal(t.calls.start.length, 0);
});

test("explicit Stop from native idle wins over the first in-flight snapshot", async () => {
  const t = setup();
  let resolve!: (runs: Pick<Run, "id" | "bot_id" | "state">[]) => void;
  t.snapshot(() => new Promise((done) => { resolve = done; }));
  const restoring = t.bridge.restoreRuns();
  await t.bridge.onNativeState("idle");
  resolve([{ id: "existing", bot_id: "b1", state: "running" }]);
  await restoring;
  assert.deepEqual(t.calls.cancel, ["existing"]);
  assert.equal(t.calls.start.length, 0);
  assert.deepEqual(t.bridge.liveRuns(), []);
});

test("Stop intent survives a failed first snapshot until a successful retry", async () => {
  const t = setup();
  let reject!: (error: Error) => void;
  t.snapshot(() => new Promise((_, fail) => { reject = fail; }));
  const restoring = t.bridge.restoreRuns();
  await t.bridge.onNativeState("idle");
  reject(new Error("offline"));
  await restoring;
  t.snapshot(async () => [{ id: "existing", bot_id: "b1", state: "running" }]);
  await t.bridge.restoreRuns();
  assert.deepEqual(t.calls.cancel, ["existing"]);
  assert.equal(t.calls.start.length, 0);
  assert.deepEqual(t.bridge.liveRuns(), []);
});

test("an automatic finish-to-idle event cannot cancel other runs in the pending snapshot", async () => {
  const t = setup();
  let resolve!: (runs: Pick<Run, "id" | "bot_id" | "state">[]) => void;
  t.snapshot(() => new Promise((done) => { resolve = done; }));
  const restoring = t.bridge.restoreRuns();
  await t.feed({ type: "run_started", run_id: "new", bot_id: "b1", chat_id: "c" });
  await t.feed({ type: "run_finished", run_id: "new", error: null });
  await t.bridge.onNativeState("idle", t.calls.stop);
  resolve([{ id: "other", bot_id: "b1", state: "running" }]);
  await restoring;
  assert.deepEqual(t.bridge.liveRuns(), ["other"]);
  assert.deepEqual(t.calls.cancel, []);
  assert.equal(t.calls.start.length, 2);
});
