import assert from "node:assert/strict";
import { test } from "node:test";
import type { ControlModeName, ControlStateDto, DaemonEvent } from "../api/types.ts";
import { createInitialLiveViewState, shouldOverrideOnStop } from "../lib/liveViewState.ts";
import { ControlBridge, type ScopeAnswer } from "./controlBridge.ts";
import { FakeClock, FakeDaemon, FakeNative, flush, makeLedger } from "./ledgerFakes.ts";
import type { StopOrigin } from "./runLedger.ts";
import type { NativeControlSnapshot } from "./stopHost.ts";
import { describeStop } from "./stopStatus.ts";
import { LIST_STALE_MS, StopRuntime } from "./stopRuntime.ts";
import { TakeoverStop } from "./takeoverStop.ts";

/**
 * The installed wiring: StopRuntime (what ControlBridgeHost forwards
 * to) + the real ControlBridge + the real RunLedger + the real TakeoverStop, with fakes
 * only at the native (Tauri commands / events) and daemon (HTTP) boundary.
 */
function host(opts: { scope?: () => ScopeAnswer; runs?: [string, "running" | "paused"][] } = {}) {
  const clock = new FakeClock();
  const native = new FakeNative();
  const { ledger } = makeLedger({ clock, native });
  const daemon = new FakeDaemon();
  for (const [id, state] of opts.runs ?? []) {
    daemon.start(id, state);
  }
  let readFails = false;
  const runtime = new StopRuntime({
    ledger,
    readNative: async () =>
      (readFails ? { mode: "idle" } : native.snapshot()) as NativeControlSnapshot,
    now: () => clock.now()
  });
  // LiveViewContext's takeover half, with the daemon's control endpoint faked per client.
  let currentClient: string | null = "A";
  let control: ControlStateDto | null = null;
  let controlFor: string | null = null;
  let overrideMode: "answer" | "hold" | "hang" = "answer";
  const overrides: string[] = [];
  const overrideCalls: { client: string; resolve: (d: ControlStateDto) => void }[] = [];
  const applied: string[] = [];
  const takeover = new TakeoverStop<string>({
    client: () => currentClient,
    controlled: () =>
      control && currentClient !== null && controlFor === currentClient
        ? shouldOverrideOnStop(createInitialLiveViewState(null), control)
        : null,
    post: (c, signal) =>
      new Promise<ControlStateDto>((resolve, reject) => {
        overrides.push(c);
        overrideCalls.push({ client: c, resolve });
        if (overrideMode === "hang") {
          return; // never settles, even when aborted
        }
        signal.addEventListener("abort", () => reject(new Error("aborted")));
        if (overrideMode === "answer") {
          resolve(control as ControlStateDto);
        }
      }),
    apply: (c) => void applied.push(c),
    schedule: (fn, ms) => clock.set(fn, ms),
    cancel: (handle) => clock.clear(handle)
  });
  runtime.setOverride((o) => takeover.stop(!o.onlyIfControlled));

  const listCalls: { resolve: () => void }[] = [];
  let holdLists = false;
  const client = {
    stopRuns: daemon.transport().stopRuns,
    listRuns: () =>
      new Promise<ReturnType<FakeDaemon["list"]>>((resolve) => {
        const snapshot = daemon.list(); // captured when the daemon handles the request
        const done = () => resolve(snapshot);
        listCalls.push({ resolve: done });
        if (!holdLists) {
          done();
        }
      })
  };
  const calls = { start: 0, decide: [] as string[], clicks: 0 };
  let clickGate: Promise<void> | null = null;
  const scope = opts.scope ?? (() => "local_desktop" as ScopeAnswer);
  const makeBridge = () =>
    new ControlBridge({
      client: {
        putControlState: async () => undefined,
        decideControl: async (_id, decision) => void calls.decide.push(decision)
      },
      native: {
        beginAction: async () => native.mode === "bot_driving",
        startRun: async () => {
          calls.start += 1;
          const snap = native.start();
          runtime.onNativeSnapshot(snap as NativeControlSnapshot);
          return snap.runGeneration;
        },
        finishRun: async (g) => {
          runtime.onNativeSnapshot(native.finish(g) as NativeControlSnapshot);
        },
        click: async () => {
          calls.clicks += 1;
          await clickGate;
        },
        confirmDriving: () => runtime.confirmDriving()
      },
      ledger,
      resolveTargetScope: () => scope(),
      botMeta: (id) => ({ botId: id, botName: id }),
      startingStep: "x",
      now: () => 1_000_000,
      sleep: async () => {
        clock.advance(250);
      }
    });
  /** The host's native subscription: deliver queued events. */
  const pumpNative = () => {
    for (const snap of native.events.splice(0)) {
      runtime.onNativeSnapshot(snap as NativeControlSnapshot);
    }
  };
  let gseq = 0;
  const event = (e: DaemonEvent) => {
    gseq += 1;
    runtime.onDaemonEvent({ v: 1, seq: gseq, gseq, ts_ms: 1_000_000, event: e });
  };
  return {
    clock,
    native,
    ledger,
    daemon,
    runtime,
    takeover,
    overrides,
    calls,
    listCalls,
    makeBridge,
    pumpNative,
    event,
    setControl: (d: ControlStateDto | null) => {
      control = d;
      controlFor = currentClient;
      takeover.pump();
    },
    /** LiveViewContext's client changed (its snapshot no longer applies). */
    switchClient: (name: string | null) => {
      currentClient = name;
      controlFor = null;
      takeover.pump();
    },
    overrideMode: (m: "answer" | "hold" | "hang") => {
      overrideMode = m;
    },
    overrideCalls,
    applied,
    /** Deliver only the native event at `index` of the queue (others stay withheld). */
    deliverNativeOnly: (index: number) => {
      const [snap] = native.events.splice(index, 1);
      if (snap) {
        runtime.onNativeSnapshot(snap as NativeControlSnapshot);
      }
    },
    holdLists: (v: boolean) => {
      holdLists = v;
    },
    failReads: (v: boolean) => {
      readFails = v;
    },
    holdClick: () => {
      let release: () => void = () => {};
      clickGate = new Promise((r) => {
        release = r;
      });
      return () => release();
    },
    client,
    status: () =>
      describeStop({
        records: ledger.stopRecords(),
        runs: ledger.runEntries(),
        now: clock.now(),
        connected: ledger.client() !== null
      })
  };
}

function activeTakeover(): ControlStateDto {
  return {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: "human_driving",
    mode: "human_driving",
    release: null,
    takeover: {
      id: "to-1",
      control_epoch: 1,
      input_expires_in_ms: 10_000,
      owner: "local-ui:1",
      session_id: "s",
      state: "active",
      target_id: "t"
    }
  };
}

type Press = (h: ReturnType<typeof host>) => void;
const ENTRY_POINTS: [string, Press, StopOrigin, boolean][] = [
  ["App hotkey (requestStop)", (h) => void h.runtime.requestStop("hotkey"), "hotkey", true],
  ["run banner (requestStop)", (h) => void h.runtime.requestStop("banner"), "banner", true],
  ["live viewer (requestStop)", (h) => void h.runtime.requestStop("viewer"), "viewer", true],
  [
    "widget / tray / global hotkey (native stop_all, event through the host subscription)",
    (h) => {
      h.native.userStop(null);
      h.pumpNative();
    },
    "native",
    false
  ]
];

test("every Stop entry point through the installed host: one decision, native halted, daemon asked, takeover input halted", async () => {
  for (const mode of ["bot_driving", "human_driving", "paused", "idle"] as ControlModeName[]) {
    for (const [name, press, origin, tokened] of ENTRY_POINTS) {
      const h = host({ runs: [["r1", "paused"]] });
      h.native.mode = mode;
      await h.runtime.readNative();
      const detach = h.runtime.attach(h.client, h.makeBridge());
      await flush();
      h.setControl(activeTakeover());
      press(h);
      h.pumpNative();
      await flush();
      const label = `${name} @ ${mode}`;
      const recs = h.ledger.stopRecords();
      assert.equal(recs.length, 1, label);
      assert.equal(recs[0]?.origin, origin, label);
      assert.equal(h.native.mode, "idle", `${label}: native halted`);
      assert.equal(h.native.tokens.length, 1, `${label}: one native stop`);
      assert.equal(h.native.tokens[0] === recs[0]?.id, tokened, `${label}: token`);
      assert.equal(h.daemon.requests.length, 1, `${label}: one daemon request`);
      assert.equal(h.ledger.mayAct("r1"), false, `${label}: run held`);
      assert.equal(h.takeover.inputAllowed(), false, `${label}: takeover input halted`);
      assert.equal(h.overrides.length, 1, `${label}: takeover override sent once`);
      h.daemon.deliver(0);
      await flush();
      assert.equal(h.status()?.kind, "stopped", label);
      assert.deepEqual(h.native.acks, [1], `${label}: acked after terminal`);
      detach();
    }
  }
});

test("chat Stop through the host cancels only its run, with no native or takeover effect", async () => {
  const h = host({ runs: [["r1", "running"], ["r2", "running"]] });
  await h.runtime.readNative();
  h.runtime.attach(h.client, h.makeBridge());
  await flush();
  h.runtime.cancelOne("r1");
  assert.deepEqual(h.daemon.requests[0]?.body.run_ids, ["r1"]);
  assert.equal(h.native.tokens.length, 0);
  assert.equal(h.takeover.inputAllowed(), true);
  assert.equal(h.ledger.mayAct("r2"), true);
});

test("a Stop during a local click through the host refuses the pending proceed", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, h.makeBridge());
  await flush();
  const release = h.holdClick();
  h.event({ type: "control_requested", request_id: "q", run_id: "r", bot_id: "b", rect: { x: 0, y: 0, width: 2, height: 2 }, label: "Go", action: "click" });
  await flush();
  assert.equal(h.calls.clicks, 1);
  h.runtime.requestStop("banner");
  release();
  await flush();
  assert.deepEqual(h.calls.decide, ["refuse"]);
});

test("the global hotkey firing during a local action is caught by the host's last-moment read", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, h.makeBridge());
  await flush();
  const release = h.holdClick();
  h.event({ type: "control_requested", request_id: "q", run_id: "r", bot_id: "b", rect: { x: 0, y: 0, width: 2, height: 2 }, label: "Go", action: "click" });
  await flush();
  h.native.userStop(null); // native stopped; its event is not delivered yet
  release();
  await flush();
  assert.deepEqual(h.calls.decide, ["refuse"]);
  assert.equal(h.ledger.stopRecords().length, 1, "the read revealed the Stop");
});

test("an older native revision never reaches the bridge or the host cache", async () => {
  const h = host();
  h.native.mode = "bot_driving";
  const older = { ...h.native.touch(), mode: "bot_driving" } as NativeControlSnapshot; // rev 1
  h.native.mode = "idle";
  const newer = { ...h.native.touch(), mode: "idle" } as NativeControlSnapshot; // rev 2
  h.runtime.onNativeSnapshot(newer);
  h.runtime.onNativeSnapshot(older); // delivered late
  assert.equal(h.runtime.nativeSnapshot()?.mode, "idle");
  const bridge = h.makeBridge();
  h.runtime.attach(h.client, bridge); // a replacement bridge starts from the cache
  await flush();
  assert.equal(bridge.mode, "idle");
});

test("a failed native read is never applied as Idle: the human keeps the wheel", async () => {
  const h = host({ runs: [["r1", "running"]] });
  const bridge = h.makeBridge();
  h.runtime.attach(h.client, bridge);
  await flush();
  h.native.takeWheel();
  h.pumpNative();
  await flush();
  assert.equal(bridge.mode, "human_driving");
  const startsBefore = h.calls.start;
  h.failReads(true);
  await h.runtime.readNative(); // getControlState() failed -> INITIAL fallback
  await flush();
  assert.equal(bridge.mode, "human_driving");
  h.runtime.scopeChanged(); // restore must not take the wheel back
  await flush();
  assert.equal(h.calls.start, startsBefore);
});

test("the host keeps one run list in flight; a stale one does not block forever", async () => {
  const h = host();
  h.holdLists(true);
  h.runtime.attach(h.client, null);
  h.runtime.tick();
  h.runtime.tick();
  assert.equal(h.listCalls.length, 1, "one list at a time");
  h.clock.advance(LIST_STALE_MS + 1);
  h.runtime.tick();
  assert.equal(h.listCalls.length, 2, "a hung list is superseded (tickets keep it ordered)");
});

test("restore drives a local run once the scope becomes known, without another daemon event", async () => {
  let scope: ScopeAnswer = "unknown";
  const h = host({ scope: () => scope, runs: [["r1", "running"]] });
  await h.runtime.readNative();
  h.runtime.attach(h.client, h.makeBridge());
  await flush();
  assert.equal(h.calls.start, 0, "list applied before targets/settings: waits");
  scope = "local_desktop";
  h.runtime.scopeChanged(); // what the host calls when settings / this client's targets load
  await flush();
  assert.equal(h.calls.start, 1);
});

test("Stop with no daemon client: input halts at once and the status says what the late Stop will cancel", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.requestStop("hotkey");
  assert.equal(h.native.mode, "idle");
  assert.equal(h.takeover.inputAllowed(), false);
  const s = h.status();
  assert.equal(s?.kind, "unreachable");
  assert.equal(s?.pendingSweep, true, "first delivery will stop every run open on arrival");
  // the override is owed until a client (and its snapshot) exists
  assert.equal(h.takeover.owes(), true);
});

// ---- late and unconfirmed Stop replies ----------------------------------------------

test("a Stop reply accepted before its native event still reaches the bridge and the cache", async () => {
  const h = host();
  h.native.mode = "bot_driving";
  h.native.touch();
  await h.runtime.readNative();
  const bridge = h.makeBridge();
  h.runtime.attach(h.client, bridge);
  await flush();
  assert.equal(bridge.mode, "bot_driving");
  h.runtime.requestStop("banner"); // native executes; its REPLY is processed first
  await flush();
  assert.equal(bridge.mode, "idle", "the bridge heard the reply's revision");
  assert.equal(h.runtime.nativeSnapshot()?.mode, "idle", "and the cache holds it");
  h.pumpNative(); // the event for the same revision arrives later: not fresh
  await h.runtime.readNative(); // a later read of the same revision
  await flush();
  const replacement = h.makeBridge();
  h.runtime.attach(h.client, replacement);
  await flush();
  assert.equal(replacement.mode, "idle", "a replacement bridge starts Idle");
});

test("a missing echo never mints another Stop or cancels a run started after the presses", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, h.makeBridge());
  await flush();
  h.native.holdReplies = true; // A's command runs natively but its reply is withheld ...
  const a = h.runtime.requestStop("banner");
  h.native.events.splice(0); // ... and so is its event (seq 1)
  h.daemon.deliver(0); // T(A) = {} fixed
  await flush();
  h.clock.advance(1000);
  h.native.userStop(null); // an independent native Stop B: seq 2
  h.deliverNativeOnly(0);
  await flush();
  h.daemon.deliver(); // T(B) = {} fixed
  await flush();
  h.clock.advance(500);
  h.daemon.start("R", "running"); // started after both target sets were fixed
  h.event({ type: "run_started", run_id: "R", chat_id: "c", bot_id: "b" });
  await flush();
  h.clock.advance(60_000); // far past any former echo timeout
  await flush();
  const roots = h.ledger.stopRecords().filter((r) => r.kind === "all" && r.supersedes === null);
  assert.equal(roots.length, 2, "two presses, two decisions");
  assert.ok(roots.some((r) => r.id === a.id));
  assert.equal(h.ledger.mayAct("R"), true);
  assert.equal(h.daemon.tokenFired.has("R"), false, "R was never cancelled");
  assert.ok(h.daemon.requests.every((r) => roots.some((x) => x.id === r.body.stop_id)));
  assert.deepEqual(h.native.acks, [], "seq 1 stays unconfirmed (no ack past it)");
});

test("a permanently hung override does not block the replacement client or a later Stop", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  h.setControl(activeTakeover());
  h.overrideMode("hang");
  h.runtime.requestStop("hotkey");
  assert.deepEqual(h.overrides, ["A"]);
  h.switchClient("B");
  h.overrideMode("answer");
  h.setControl(activeTakeover()); // B's snapshot shows a takeover still active
  assert.deepEqual(h.overrides, ["A", "B"], "delivered through B despite A hanging");
  await flush();
  assert.equal(h.takeover.owes(), false);
  // same client, hung again: a later Stop is not blocked either
  h.overrideMode("hang");
  h.clock.advance(1000); // a separate press, not the same key press
  h.runtime.requestStop("banner");
  h.clock.advance(1000);
  h.overrideMode("answer");
  h.runtime.requestStop("banner");
  assert.equal(h.overrides.length, 4);
  assert.equal(h.takeover.inputAllowed(), false, "local input stays disabled meanwhile");
});

test("an unconfirmed Stop requests its own reconciliation and keeps its status until resolved", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  await flush();
  const listsBefore = h.listCalls.length;
  const a = h.runtime.requestStop("banner");
  h.pumpNative();
  h.daemon.processAndLose(0); // the daemon fixed T(A); the reply was lost
  await flush();
  h.daemon.forgetBook(); // TTL: the id is remembered, its target set is not
  h.daemon.start("R", "running"); // a run seen while the uncertainty lasts
  h.clock.advance(500); // retry -> expired
  h.daemon.deliver();
  await flush();
  assert.equal(h.ledger.stop(a.id)?.phase, "unconfirmed");
  assert.ok(h.listCalls.length > listsBefore, "a reconciliation list was requested by the runtime itself");
  await flush();
  assert.equal(h.ledger.mayAct("R"), false, "R may be in the forgotten T: quarantined");
  // a newer Stop does not hide the older uncertainty while it is unresolved
  const b = h.runtime.requestStop("hotkey");
  assert.equal(h.status()?.kind, "unconfirmed");
  assert.equal(h.status()?.stopId, a.id);
  // B sweeps everything open now: the evidence A was waiting for
  h.daemon.deliver();
  await flush();
  assert.equal(h.ledger.stop(a.id)?.phase, "settled");
  assert.equal(h.status()?.stopId, b.id, "now the latest Stop is shown");
  assert.equal(h.ledger.run("R")?.phase, "cancel_requested", "R is held by B, not released");
});

test("a delayed override answer cannot pause a takeover the user acquired afterwards", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  h.overrideMode("hold");
  h.runtime.requestStop("viewer"); // forced override, processed but its answer delayed
  assert.deepEqual(h.overrides, ["A"]);
  h.takeover.resume(); // the user reacquires (a newer explicit decision) ...
  h.switchClient("B"); // ... and reconnects as B
  h.setControl(activeTakeover()); // B: the new takeover is active
  h.overrideCalls[0]?.resolve(activeTakeover()); // A finally answers
  await flush();
  assert.deepEqual(h.overrides, ["A"], "never re-sent through B");
  assert.deepEqual(h.applied, [], "and A's answer is not applied");
  assert.equal(h.takeover.inputAllowed(), true);
});

test("connected but unsynced, the late-sweep warning is shown at once (en + tr text)", async () => {
  const { readFileSync } = await import("node:fs");
  const { stopDetail } = await import("./stopStatus.ts");
  const h = host();
  h.holdLists(true); // the first run list has not arrived: unsynced
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  h.runtime.requestStop("banner");
  const s = h.status();
  assert.ok(s);
  assert.equal(s.kind, "stopping", "connected, request in flight");
  const d = stopDetail(s);
  const text = (locale: string) => {
    const dict = JSON.parse(readFileSync(new URL(`../i18n/locales/${locale}.json`, import.meta.url), "utf8")) as Record<string, unknown>;
    return d.key.split(".").reduce<unknown>((o, k) => (o as Record<string, unknown>)[k], dict) as string;
  };
  assert.match(text("en"), /EVERY run open when the background service receives it, including runs started meanwhile/);
  assert.match(text("tr"), /TÜM çalışmaları durduracak; arada başka bir cihazdan veya bir zamanlamadan başlatılanlar dahil/);
});

test("with the reconciliation list hung, a later user Stop that sweeps resolves the older uncertainty", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  await flush();
  const a = h.runtime.requestStop("banner");
  h.pumpNative();
  h.daemon.processAndLose(0);
  await flush();
  h.daemon.forgetBook();
  h.daemon.start("R", "running");
  h.holdLists(true); // the reconciliation list never comes back
  h.clock.advance(500);
  h.daemon.deliver(); // expired -> unconfirmed
  await flush();
  assert.equal(h.ledger.stop(a.id)?.phase, "unconfirmed");
  assert.deepEqual(h.native.acks, []);
  h.clock.advance(1000);
  h.runtime.requestStop("banner"); // the user's "Stop all now": a new all_existing decision
  h.pumpNative();
  h.daemon.deliver(); // first receipt sweeps every run open now (R included)
  await flush();
  assert.equal(h.ledger.stop(a.id)?.phase, "settled", "the sweep is the evidence A waited for");
  assert.deepEqual(h.native.acks, [1], "A's sequence is acknowledged; B's waits for R");
});

test("a later sweeping Stop resolves an older unconfirmed Stop's runs that already finished", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  await flush();
  const a = h.runtime.requestStop("banner"); // all_existing, unreached
  h.pumpNative();
  h.daemon.processAndLose(0); // the daemon fixed T(A); the reply was lost
  await flush();
  h.daemon.forgetBook(); // TTL: A's id is remembered, its target set is not
  h.clock.advance(1000);
  h.clock.advance(1000);
  h.runtime.requestStop("hotkey"); // all_existing too, still in flight below
  h.pumpNative();
  await flush();
  h.daemon.start("R", "running");
  h.event({ type: "run_started", run_id: "R", chat_id: "c", bot_id: "b" }); // quarantined by the older A
  await flush();
  assert.equal(h.ledger.run("R")?.quarantinedBy, a.id);
  h.daemon.deliver(3); // A's retry: the daemon forgot the id -> expired before T was known
  await flush();
  assert.equal(h.ledger.stop(a.id)?.phase, "unconfirmed");
  assert.equal(h.ledger.run("R")?.quarantinedBy, a.id, "R stays held by the older Stop");
  h.holdLists(true); // no list ever answers
  h.daemon.end("R"); // R finished; its run_finished event was lost
  h.daemon.deliver(2); // B's first receipt sweeps what is open now: R is not in it
  await flush();
  assert.equal(h.ledger.stop(a.id)?.phase, "settled", "A resolves on the sweep's evidence");
  assert.equal(h.ledger.run("R")?.quarantinedBy ?? null, null, "nothing is left held by A");
  assert.ok(h.status()?.kind !== "unconfirmed", "no stuck 'not confirmed' notice");
});

test("a user's Clear pause / release retires the owed override; input stays halted", async () => {
  const h = host();
  await h.runtime.readNative();
  h.runtime.attach(h.client, null);
  h.overrideMode("hang"); // the override never completes: still owed
  h.runtime.requestStop("viewer");
  assert.deepEqual(h.overrides, ["A"]);
  assert.equal(h.takeover.owes(), true);
  h.takeover.retire(); // clearPause / performRelease
  assert.equal(h.takeover.owes(), false);
  h.setControl(activeTakeover()); // a snapshot arrives after the user's decision
  h.switchClient("B");
  h.setControl(activeTakeover());
  assert.deepEqual(h.overrides, ["A"], "the stale override is never re-sent");
  assert.equal(h.takeover.inputAllowed(), false, "only taking control again re-enables input");
});

test("LiveViewContext retires on Clear pause / release and pumps only after the state ref is current", async () => {
  const { readFileSync } = await import("node:fs");
  const src = readFileSync(new URL("./LiveViewContext.tsx", import.meta.url), "utf8");
  const body = (name: string, next: string) => {
    const from = src.indexOf(name);
    assert.ok(from >= 0, name);
    return src.slice(from, src.indexOf(next, from));
  };
  assert.match(body("const clearPauseInner = useCallback", "const clearPause = useCallback"), /takeoverStopRef\.current\?\.retire\(\)/);
  assert.match(body("const performRelease = useCallback", "const releaseTakeover"), /takeoverStopRef\.current\?\.retire\(\)/);
  const ingest = body("const ingestControlSnapshot = useCallback", "const issueSnapshotTicket");
  assert.ok(ingest.indexOf("updateState(") < ingest.indexOf("pump()"), "ingest: state before pump");
  const refresh = body("const refreshTargets = useCallback", "// Refresh targets");
  assert.ok(refresh.indexOf("updateState((prev) => applyControlStateDto(prev, nextCtrl))") < refresh.indexOf("pump()"));
  assert.doesNotMatch(body("const setControlState = useCallback", "const takeoverStopRef"), /pump\(\)/);
});
