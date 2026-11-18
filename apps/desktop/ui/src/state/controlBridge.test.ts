import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  ControlModeName,
  ControlStateDto,
  ControlVerdict,
  DaemonEvent,
  GlobalEnvelope,
  ReleaseRequest
} from "../api/types.ts";
import {
  type BotMeta,
  ControlBridge,
  createBridgeRegistry,
  type PanelActionHighlight,
  type Rect,
  refusalReason,
  resolveTargetScope,
  SCOPE_UNKNOWN_REASON,
  type ScopeAnswer
} from "./controlBridge.ts";
import { FakeDaemon, flush, makeLedger } from "./ledgerFakes.ts";

const NOW = 1_000_000;

interface Calls {
  put: ControlModeName[];
  release: boolean[];
  releaseTakeover: ReleaseRequest[];
  override: number;
  recover: number;
  decide: { id: string; decision: ControlVerdict; reason?: string }[];
  begin: { rect: Rect | null; label: string; action: string }[];
  start: { bot: BotMeta; step: string }[];
  finish: number[];
  clicks: { x: number; y: number }[];
  panelOverlay: PanelActionHighlight[];
}

function idleDto(): ControlStateDto {
  return {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: null,
    mode: "idle",
    release: null,
    takeover: null
  };
}

/**
 * A bridge over the REAL run ledger (fake daemon + fake native), so the tests
 * exercise the bridge <-> ledger wiring, not a stub of it.
 */
function setup(
  beginResult: boolean | (() => boolean) = true,
  scopeResolver?: () => ScopeAnswer,
  overrides: {
    startRun?: (bot: BotMeta, step: string) => Promise<number | void>;
    beginAction?: () => Promise<boolean>;
    click?: () => Promise<void>;
    confirmDriving?: () => Promise<boolean>;
    client?: Partial<ConstructorParameters<typeof ControlBridge>[0]["client"]>;
  } = {}
) {
  const calls: Calls = {
    put: [],
    release: [],
    releaseTakeover: [],
    override: 0,
    recover: 0,
    decide: [],
    begin: [],
    start: [],
    finish: [],
    clicks: [],
    panelOverlay: []
  };
  let now = NOW;
  const w = makeLedger();
  w.ledger.onNativeSnapshot(w.native.snapshot());
  const daemon = new FakeDaemon();
  const handle = w.ledger.attachClient(daemon.transport());
  const bridge = new ControlBridge({
    client: {
      putControlState: async (m, release) => {
        calls.put.push(m);
        calls.release.push(release === true);
      },
      decideControl: async (id, decision, reason) =>
        void calls.decide.push({ id, decision, reason }),
      releaseControl: async (req) => {
        if (req) {
          calls.releaseTakeover.push(req);
        }
        return idleDto();
      },
      overrideControl: async () => {
        calls.override += 1;
        return { ...idleDto(), latched: "paused", mode: "paused" };
      },
      recoverControl: async () => {
        calls.recover += 1;
        return idleDto();
      },
      ...overrides.client
    },
    native: {
      beginAction: async (rect, label, action) => {
        calls.begin.push({ rect, label, action });
        if (overrides.beginAction) {
          return overrides.beginAction();
        }
        return typeof beginResult === "function" ? beginResult() : beginResult;
      },
      startRun: async (bot, step) => {
        calls.start.push({ bot, step });
        if (overrides.startRun) {
          return overrides.startRun(bot, step);
        }
        w.native.mode = "bot_driving";
        w.native.runGeneration += 1;
        return w.native.runGeneration;
      },
      finishRun: async (generation) => {
        calls.finish.push(generation);
        w.native.finish(generation);
      },
      click: async (x, y) => {
        calls.clicks.push({ x, y });
        await overrides.click?.();
      },
      confirmDriving: overrides.confirmDriving
    },
    ledger: w.ledger,
    resolveTargetScope: scopeResolver,
    panelOverlay: {
      showAction: (h) => calls.panelOverlay.push(h)
    },
    botMeta: (id) => ({ botId: id, botName: `Bot ${id}` }),
    startingStep: "Working…",
    now: () => now,
    sleep: async () => {
      now += 250;
    }
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
    ledger: w.ledger,
    native: w.native,
    daemon,
    handle,
    advance: (ms: number) => {
      now += ms;
    }
  };
}

const request = (id: string, rect: Rect | null = null, action = "click", runId = "run-1") =>
  ({
    type: "control_requested",
    request_id: id,
    run_id: runId,
    bot_id: "b1",
    rect,
    label: "Save",
    action
  }) satisfies DaemonEvent;

const started = (runId: string): DaemonEvent => ({
  type: "run_started",
  run_id: runId,
  chat_id: "c",
  bot_id: "b1"
});

test("a started run makes the bot drive; the last finished run ends it with finish_bot_run, never a Stop", async () => {
  const t = setup();
  await t.feed(started("run-1"));
  assert.equal(t.calls.start.length, 1);
  assert.equal(t.calls.start[0].bot.botName, "Bot b1");
  assert.equal(t.calls.start[0].step, "Working…");
  assert.equal(t.bridge.mode, "bot_driving");
  await t.feed(started("run-2"));
  assert.equal(t.calls.start.length, 1, "a second run does not restart it");
  await t.feed({ type: "run_finished", run_id: "run-1", error: null });
  assert.deepEqual(t.calls.finish, [], "run-2 is still live");
  await t.feed({ type: "run_finished", run_id: "run-2", error: null });
  assert.deepEqual(t.calls.finish, [1], "finished with the generation it started");
  assert.equal(t.bridge.mode, "idle");
  assert.deepEqual(t.native.tokens, [], "no user stop was issued");
  assert.equal(t.native.stopSeq, 0);
  assert.equal(t.ledger.stopRecords().length, 0);
});

test("finish with a stale generation keeps a newer native run driving", async () => {
  const t = setup();
  await t.feed(started("run-1"));
  t.native.runGeneration += 1; // another native run (e.g. dev simulate) took over
  t.native.mode = "bot_driving";
  await t.feed({ type: "run_finished", run_id: "run-1", error: null });
  assert.deepEqual(t.calls.finish, [1]);
  assert.equal(t.native.mode, "bot_driving", "finish_bot_run(1) was a no-op natively");
});

test("events from before the bridge existed (history replay) are ignored", async () => {
  const t = setup();
  await t.feed(started("old"), NOW - 60_000);
  assert.equal(t.calls.start.length, 0);
  assert.equal(t.ledger.run("old"), undefined);
});

test("control_requested shows the target, posts proceed and ripples a click", async () => {
  const t = setup(true);
  const rect = { x: 100, y: 200, width: 40, height: 20 };
  await t.feed(request("r1", rect));
  assert.deepEqual(t.calls.begin, [{ rect, label: "Save", action: "click" }]);
  assert.deepEqual(t.calls.decide, [{ id: "r1", decision: "proceed", reason: undefined }]);
  assert.deepEqual(t.calls.clicks, [{ x: 120, y: 210 }]);
  assert.equal(t.calls.start.length, 1, "the request arrived while idle: the run state starts first");
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
  assert.deepEqual(t.calls.put, ["bot_driving", "human_driving", "human_driving"]);
});

test("only the user's explicit resume sends release:true, never a heartbeat or reopen", async () => {
  const t = setup();
  await t.bridge.heartbeat();
  await t.bridge.onNativeState("human_driving");
  await t.bridge.heartbeat();
  await t.bridge.heartbeat();
  assert.deepEqual(t.calls.release, [false, false, false, false]);
  await t.bridge.onNativeState("idle");
  assert.equal(t.calls.release.at(-1), true);
  await t.bridge.heartbeat();
  assert.equal(t.calls.release.at(-1), false, "later heartbeats do not release");
  await t.bridge.onNativeState("paused");
  assert.equal(t.calls.release.at(-1), false);
  await t.bridge.onNativeState("bot_driving");
  assert.equal(t.calls.release.at(-1), true);
});

test("the bridge never cancels: native idle (Stop, finish or hand-back) sends no cancel by itself", async () => {
  const t = setup();
  await t.feed(started("run-1"));
  await t.bridge.onNativeState("bot_driving");
  await t.bridge.onNativeState("idle");
  await t.bridge.onNativeState("human_driving");
  await t.bridge.onNativeState("idle");
  assert.equal(t.daemon.requests.length, 0, "only Stop records (the ledger) cancel");
  assert.equal(t.ledger.mayAct("run-1"), true);
});

test("a Stop-held run cannot act again: its next control_requested is refused without touching native", async () => {
  const t = setup();
  await t.feed(started("run-1"));
  await t.bridge.onNativeState("idle"); // the Stop's native echo
  t.ledger.stopAll("banner");
  await t.feed(started("run-1")); // another event for the same run
  await t.feed(request("after-stop", { x: 0, y: 0, width: 10, height: 10 }));
  assert.equal(t.calls.start.length, 1, "no restart of the overlay");
  assert.equal(t.calls.begin.length, 0);
  assert.equal(t.calls.clicks.length, 0);
  assert.deepEqual(t.calls.decide.map((d) => d.decision), ["refuse"]);
  assert.match(t.calls.decide[0].reason ?? "", /stopped/);
});

test("a failed cancel keeps the run stop-held and its next control request is refused", async () => {
  const t = setup();
  t.daemon.start("run-1", "paused");
  t.daemon.failCancel.add("run-1");
  await t.feed(started("run-1"));
  t.ledger.stopAll("hotkey");
  t.daemon.deliver(0);
  await flush();
  assert.equal(t.ledger.run("run-1")?.phase, "failed");
  await t.feed(request("again"));
  assert.equal(t.calls.begin.length, 0);
  assert.equal(t.calls.decide.at(-1)?.decision, "refuse");
});

test("a Stop pressed between beginAction and the decision turns the decision into a refusal", async () => {
  let ledgerRef: ReturnType<typeof setup>["ledger"] | null = null;
  const t = setup(true, undefined, {
    beginAction: async () => {
      ledgerRef?.stopAll("hotkey");
      return true;
    }
  });
  ledgerRef = t.ledger;
  await t.feed(request("racy", { x: 0, y: 0, width: 4, height: 4 }));
  assert.equal(t.calls.clicks.length, 0, "no click after the Stop");
  assert.equal(t.calls.decide[0]?.decision, "refuse");
});

test("revoke during the ensureDriving await prevents beginAction and proceed", async () => {
  let resume: () => void = () => {};
  const t = setup(true, undefined, {
    startRun: () => new Promise((resolve) => {
      resume = () => resolve(1);
    })
  });
  const answered = t.feed(request("r-revoke"));
  await flush();
  assert.equal(t.calls.start.length, 1);
  t.bridge.revoke();
  resume();
  await answered;
  assert.equal(t.calls.begin.length, 0);
  assert.equal(t.calls.decide.length, 0, "a revoked bridge writes nothing");
});

test("revoke is immediate even with a hung native call: nothing of the old session acts afterwards", async () => {
  const t = setup(true, undefined, { startRun: () => new Promise(() => {}) });
  void t.feed(request("hung"));
  await flush();
  t.bridge.revoke();
  assert.equal(t.bridge.live, false);
  await t.bridge.heartbeat();
  void t.bridge.onNativeState("bot_driving");
  await flush();
  assert.deepEqual(t.calls.put, [], "no heartbeat after revoke");
});

test("Stop is sent while a hand-back is pending, and the late release reply is harmless", async () => {
  let finishRelease: (dto: ControlStateDto) => void = () => {};
  const t = setup(true, undefined, {
    client: {
      releaseControl: () => new Promise((resolve) => {
        finishRelease = resolve;
      })
    }
  });
  t.daemon.start("run-1", "paused");
  await t.feed(started("run-1"));
  const release = t.bridge.releaseOwnedTakeover({
    takeover_id: "to-1",
    control_epoch: 1,
    request_id: "rel",
    outcome: "completed"
  });
  await flush(); // the release is now in flight on the bridge queue
  t.ledger.stopAll("banner");
  assert.equal(t.daemon.requests.length, 1, "not queued behind the hand-back");
  finishRelease(idleDto());
  await release;
  t.daemon.deliver(0);
  await flush();
  assert.equal(t.ledger.mayAct("run-1"), false);
});

test("Stop is sent while a getControlState poll never resolves; the hung poll does not hold the bridge", async () => {
  const t = setup(true, undefined, {
    client: { getControlState: () => new Promise(() => {}) }
  });
  (t.bridge as unknown as { deps: { callBoundMs: number } }).deps.callBoundMs = 10;
  void t.bridge.refreshControlState();
  t.ledger.stopAll("hotkey");
  assert.equal(t.daemon.requests.length, 1);
  await t.bridge.onNativeState("bot_driving"); // queued behind the hung poll, freed by the bound
  assert.deepEqual(t.calls.put, ["bot_driving"]);
});

test("unknown target scope never opens the local overlay; the action is refused with 'retry in a moment'", async () => {
  let scope: ScopeAnswer = "unknown";
  const t = setup(true, () => scope);
  await t.feed(started("run-x"));
  assert.equal(t.calls.start.length, 0, "run_started with unknown scope does not drive native");
  await t.feed(request("r-unknown", { x: 1, y: 1, width: 2, height: 2 }, "click", "run-x"));
  assert.equal(t.calls.start.length, 0);
  assert.equal(t.calls.begin.length, 0);
  assert.deepEqual(t.calls.decide, [{ id: "r-unknown", decision: "refuse", reason: SCOPE_UNKNOWN_REASON }]);
  // once this client's targets have loaded the scope is known and actions proceed
  scope = "local_desktop";
  await t.feed(request("r-known", null, "click", "run-x"));
  assert.equal(t.calls.decide.at(-1)?.decision, "proceed");
});

test("restore drives restored local processing runs only (never held, paused or unknown-scope runs)", async () => {
  let scope: ScopeAnswer = "local_desktop";
  const t = setup(true, () => scope);
  t.daemon.start("live", "running");
  t.daemon.start("parked", "paused");
  t.ledger.applyList(t.ledger.beginList(t.handle)!, t.daemon.list());
  scope = "unknown";
  await t.bridge.restoreDriving();
  assert.equal(t.calls.start.length, 0, "unknown scope: wait");
  scope = "local_desktop";
  await t.bridge.restoreDriving();
  assert.equal(t.calls.start.length, 1);
  assert.deepEqual(t.bridge.drivingRuns(), ["live"]);
  // a Stop then a new restore: nothing restarts
  t.ledger.stopAll("hotkey");
  await t.bridge.onNativeState("idle");
  await t.bridge.restoreDriving();
  assert.equal(t.calls.start.length, 1);
});

test("refusal reasons name the state", () => {
  assert.match(refusalReason("human_driving"), /taken control/);
  assert.match(refusalReason("paused"), /paused/);
  assert.match(refusalReason("bot_driving"), /stopped/);
});

test("owned takeover: Bitirdim uses releaseControl, heartbeats never release, a local Pause overrides", async () => {
  const t = setup();
  await t.feed(started("run-1"));
  await t.bridge.syncControlState({
    app_connected: true,
    expires_in_ms: 15_000,
    latched: "human_driving",
    mode: "human_driving",
    release: null,
    takeover: {
      id: "to-1",
      control_epoch: 3,
      input_expires_in_ms: 10_000,
      owner: "local-ui:1",
      session_id: "sess-1",
      state: "active",
      target_id: "target-1"
    }
  });
  t.bridge.setOwnedTakeoverId("to-1");
  await t.bridge.onNativeState("human_driving");
  await t.bridge.heartbeat();
  assert.equal(t.calls.release.some(Boolean), false);

  const relReq: ReleaseRequest = {
    takeover_id: "to-1",
    control_epoch: 3,
    request_id: "req-rel-1",
    outcome: "completed",
    human_action: "login_completed"
  };
  await t.bridge.releaseOwnedTakeover(relReq);
  assert.equal(t.calls.releaseTakeover.length, 1);
  assert.equal(t.calls.releaseTakeover[0].request_id, "req-rel-1");
  assert.equal(t.daemon.requests.length, 0, "a hand-back never stops runs");
  assert.equal(t.bridge.takeover(), null);

  await t.feed({
    type: "control_state_changed",
    state: "active",
    owner: "peer:phone",
    target_id: "target-1",
    reason: null
  });
  await t.bridge.onNativeState("paused");
  assert.equal(t.calls.override, 0, "a peer's takeover is not ours");

  await t.bridge.syncControlState({
    app_connected: true,
    expires_in_ms: 15_000,
    latched: "human_driving",
    mode: "human_driving",
    release: null,
    takeover: {
      id: "to-2",
      control_epoch: 4,
      input_expires_in_ms: 10_000,
      owner: "local-ui:1",
      session_id: "sess-1",
      state: "active",
      target_id: "target-1"
    }
  });
  t.bridge.setOwnedTakeoverId("to-2");
  await t.bridge.onNativeState("idle");
  await t.bridge.onNativeState("paused");
  assert.equal(t.calls.override, 1);
});

test("browser and remote targets NEVER touch local PC overlay/clicks and use panel overlay", async () => {
  let scope: ScopeAnswer = "remote";
  const t = setup(true, () => scope);
  const rect = { x: 400, y: 300, width: 120, height: 44 };
  await t.feed(started("run-remote"));
  await t.feed({
    type: "control_requested",
    request_id: "req-remote-1",
    run_id: "run-remote",
    bot_id: "b1",
    rect,
    label: "Submit order",
    action: "click"
  });
  assert.equal(t.calls.start.length, 0, "never opens local desktop run overlay");
  assert.equal(t.calls.begin.length, 0, "never highlights local PC desktop");
  assert.equal(t.calls.clicks.length, 0, "never clicks local PC desktop");
  assert.equal(t.calls.panelOverlay.length, 1);
  assert.deepEqual(t.calls.panelOverlay[0].rect, rect);
  assert.deepEqual(t.calls.decide, [{ id: "req-remote-1", decision: "proceed", reason: undefined }]);

  scope = "browser";
  await t.feed({
    type: "control_state_changed",
    state: "paused",
    owner: "local-ui:1",
    target_id: "target-browser",
    reason: "disconnected"
  });
  await t.feed({
    type: "control_requested",
    request_id: "req-browser-2",
    run_id: "run-remote",
    bot_id: "b1",
    rect,
    label: "Next",
    action: "click"
  });
  assert.equal(t.calls.begin.length, 0);
  assert.equal(t.calls.clicks.length, 0);
  assert.equal(t.calls.decide[1].decision, "refuse");

  // a stop-held browser run is refused too, with no panel highlight
  scope = "browser";
  await t.feed({ type: "control_state_changed", state: "released", owner: "local-ui:1", target_id: "target-browser", reason: null });
  t.ledger.stopAll("viewer");
  await t.feed({
    type: "control_requested",
    request_id: "req-browser-3",
    run_id: "run-remote",
    bot_id: "b1",
    rect,
    label: "Next",
    action: "click"
  });
  assert.equal(t.calls.panelOverlay.length, 1);
  assert.equal(t.calls.decide.at(-1)?.decision, "refuse");
});

test("resolveTargetScope distinguishes local_desktop from browser and remote targets", () => {
  const localDesktop = {
    target_id: "t-local",
    bot_id: null,
    run_id: null,
    host_id: "local",
    kind: "desktop" as const,
    generation: 1,
    capabilities: { codecs: ["jpeg" as const], view: true, input: true, reason: null },
    unavailable_reason: null
  };
  const remoteDesktop = {
    ...localDesktop,
    target_id: "t-remote",
    bot_id: "bot-r",
    run_id: "run-r",
    host_id: "peer:phone-or-mac",
    unavailable_reason: "remote_node_live_view" as const
  };
  const browser = {
    ...localDesktop,
    target_id: "t-browser",
    bot_id: "bot-b",
    run_id: "run-b",
    kind: "browser" as const
  };
  assert.equal(
    resolveTargetScope({
      runId: "run-r",
      botId: "bot-r",
      targets: [localDesktop, remoteDesktop, browser],
      selectedTarget: localDesktop,
      computerTarget: "this-pc"
    }),
    "remote"
  );
  assert.equal(
    resolveTargetScope({
      runId: "run-b",
      botId: "bot-b",
      targets: [localDesktop, remoteDesktop, browser],
      selectedTarget: localDesktop,
      computerTarget: "this-pc"
    }),
    "browser"
  );
  assert.equal(
    resolveTargetScope({
      runId: "run-other",
      botId: "bot-other",
      targets: [localDesktop],
      selectedTarget: localDesktop,
      computerTarget: "this-pc"
    }),
    "local_desktop"
  );
});

test("startRun preserves explicit botShape even when it differs from ID hash fallback", async () => {
  const calls: { bot: BotMeta; step: string }[] = [];
  const { ledger } = makeLedger();
  const bridge = new ControlBridge({
    client: {
      putControlState: async () => undefined,
      decideControl: async () => undefined
    },
    native: {
      beginAction: async () => true,
      startRun: async (bot, step) => void calls.push({ bot, step }),
      finishRun: async () => undefined,
      click: async () => undefined
    },
    ledger,
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
    event: { type: "run_started", run_id: "run-shape", chat_id: "c", bot_id: "bot-cube-0" }
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].bot.botId, "bot-cube-0");
  assert.equal(calls[0].bot.botShape, "heart");
});

// ---- review of PR 56: bridge lifecycle, release failure, ownership by id ----

function owned(id: string, state = "active"): ControlStateDto {
  return {
    ...idleDto(),
    latched: "human_driving",
    mode: "human_driving",
    takeover: {
      id,
      control_epoch: 3,
      input_expires_in_ms: 10_000,
      owner: "local-ui:1",
      session_id: "sess-1",
      state: state as "active",
      target_id: "target-1"
    }
  };
}

function bareBridge(client: Partial<ConstructorParameters<typeof ControlBridge>[0]["client"]> = {}) {
  const log = { put: [] as boolean[], override: 0 };
  const { ledger } = makeLedger();
  const bridge = new ControlBridge({
    client: {
      putControlState: async (_m, release) => void log.put.push(release === true),
      decideControl: async () => undefined,
      overrideControl: async () => {
        log.override += 1;
        return idleDto();
      },
      ...client
    },
    native: {
      beginAction: async () => true,
      startRun: async () => 1,
      finishRun: async () => undefined,
      click: async () => undefined
    },
    ledger,
    botMeta: (id) => ({ botId: id, botName: id }),
    startingStep: "x",
    now: () => NOW
  });
  const feed = (event: DaemonEvent) =>
    bridge.handleEvent({ v: 1, seq: 1, ts_ms: NOW, gseq: 1, event });
  return { bridge, log, feed, ledger };
}

test("control-state refreshes keep the same bridge: its driving run and mode survive them", async () => {
  const { bridge, feed } = bareBridge();
  let latest: ControlStateDto | null = null;
  const registry = createBridgeRegistry(() => latest);
  registry.register(bridge);
  await feed({ type: "run_started", run_id: "run-1", chat_id: "c", bot_id: "b1" });
  assert.deepEqual(bridge.drivingRuns(), ["run-1"]);
  for (let i = 0; i < 4; i += 1) {
    latest = i % 2 ? idleDto() : owned("to-1");
    registry.syncControlState(latest);
    registry.setOwnedTakeoverId(i % 2 ? null : "to-1");
  }
  await bridge.syncControlState(idleDto());
  assert.equal(registry.current(), bridge, "same bridge instance");
  assert.deepEqual(bridge.drivingRuns(), ["run-1"], "not reset");
  assert.equal(bridge.mode, "bot_driving");
});

test("registering a bridge hands it the current snapshot and owned takeover id", async () => {
  const { bridge, log } = bareBridge();
  const registry = createBridgeRegistry(() => owned("to-9"));
  registry.setOwnedTakeoverId("to-9");
  registry.register(bridge);
  await bridge.syncControlState(owned("to-9"));
  await bridge.heartbeat(true);
  assert.deepEqual(log.put, [false], "owned takeover blocks the legacy release");
});

test("a failed release rejects (not null) and leaves the bridge usable", async () => {
  let fail = true;
  const { bridge } = bareBridge({
    releaseControl: async () => {
      if (fail) {
        throw new Error("409");
      }
      return idleDto();
    }
  });
  const req: ReleaseRequest = {
    takeover_id: "to-1",
    control_epoch: 3,
    request_id: "req-1",
    outcome: "completed"
  };
  await assert.rejects(() => bridge.releaseOwnedTakeover(req), /409/);
  fail = false;
  const dto = await bridge.releaseOwnedTakeover(req);
  assert.equal(dto?.takeover, null, "chain still works after a failure");
});

test("legacy release is blocked only for OUR live takeover, not a peer's or a paused one", async () => {
  const { bridge, log } = bareBridge();
  bridge.setOwnedTakeoverId("to-mine");
  await bridge.syncControlState(owned("to-peer"));
  await bridge.heartbeat(true);
  assert.deepEqual(log.put.at(-1), true, "a peer's takeover does not block");
  await bridge.syncControlState(owned("to-mine", "paused"));
  await bridge.heartbeat(true);
  assert.deepEqual(log.put.at(-1), true, "a paused takeover does not block");
  await bridge.syncControlState(owned("to-mine"));
  await bridge.heartbeat(true);
  assert.deepEqual(log.put.at(-1), false, "our live takeover blocks");
});

test("a missed control_state_changed 'released' event is healed from GET /control/state", async () => {
  let snapshot: ControlStateDto = owned("to-mine");
  const { bridge, log } = bareBridge({ getControlState: async () => snapshot });
  bridge.setOwnedTakeoverId("to-mine");
  await bridge.refreshControlState();
  await bridge.heartbeat(true);
  assert.equal(log.put.at(-1), false);
  snapshot = idleDto();
  await bridge.refreshControlState();
  await bridge.heartbeat(true);
  assert.equal(log.put.at(-1), true, "no stale tracker blocks Resume");
});

test("control_state_changed events re-read the snapshot (events carry no takeover id)", async () => {
  const { bridge, log, feed } = bareBridge({ getControlState: async () => owned("to-mine") });
  bridge.setOwnedTakeoverId("to-mine");
  await feed({
    type: "control_state_changed",
    state: "active",
    owner: "local-ui:1",
    target_id: "target-1",
    reason: null
  });
  assert.equal(bridge.takeover()?.id, "to-mine");
  await bridge.heartbeat(true);
  assert.equal(log.put.at(-1), false);
});

test("a Stop during the click await turns the pending proceed into a refusal", async () => {
  let finishClick: () => void = () => {};
  const t = setup(true, undefined, {
    click: () => new Promise<void>((resolve) => {
      finishClick = resolve;
    })
  });
  const answered = t.feed(request("click-race", { x: 0, y: 0, width: 4, height: 4 }));
  await flush();
  assert.equal(t.calls.clicks.length, 1, "the click is in flight");
  t.ledger.stopAll("banner"); // pressed while native animates the click
  finishClick();
  await answered;
  assert.deepEqual(t.calls.decide.map((d) => d.decision), ["refuse"]);
});

test("a Stop that has reached native but not the ledger yet is caught by the last-moment native read", async () => {
  let routed: (() => void) | null = null;
  const t = setup(true, undefined, {
    confirmDriving: async () => {
      routed?.();
      return false; // native says Idle: the global hotkey fired
    }
  });
  routed = () => {
    t.ledger.onNativeSnapshot(t.native.userStop(null));
  };
  await t.feed(request("hotkey-race", { x: 0, y: 0, width: 4, height: 4 }));
  assert.deepEqual(t.calls.decide.map((d) => d.decision), ["refuse"]);
  assert.equal(t.ledger.stopRecords().length, 1, "and the Stop is recorded");
});

test("any Stop decided after the action began refuses it, even one whose run set does not include it", async () => {
  let ledgerRef: ReturnType<typeof setup>["ledger"] | null = null;
  const t = setup(true, undefined, {
    beginAction: async () => {
      // the run is unknown to that Stop (seen only now), yet nothing local may proceed
      ledgerRef?.stopAll("hotkey");
      return true;
    }
  });
  ledgerRef = t.ledger;
  await t.feed(request("epoch", null, "key", "run-new"));
  assert.equal(t.calls.decide[0]?.decision, "refuse");
});

test("the same key press reaching native again during an action (a joined Stop) still refuses it", async () => {
  let pressAgain: (() => void) | null = null;
  const t = setup(true, undefined, {
    beginAction: async () => {
      pressAgain?.();
      return true;
    }
  });
  t.ledger.stopAll("hotkey");
  t.daemon.deliver(0); // answered: nothing was running
  await flush();
  // a run that started after that Stop may act ...
  pressAgain = () => {
    // ... but the global listener's copy of the same press arrives now (joins the Stop)
    t.ledger.onNativeSnapshot(t.native.userStop(null));
  };
  await t.feed(request("joined", null, "key", "run-after"));
  assert.equal(t.ledger.stopRecords().length, 1, "still one decision");
  assert.equal(t.calls.decide.at(-1)?.decision, "refuse", "nothing local proceeds after a Stop press");
});
