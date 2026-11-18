import assert from "node:assert/strict";
import { test } from "node:test";
import {
  FakeClock,
  FakeDaemon,
  FakeNative,
  flush,
  makeLedger,
  MemoryStorage
} from "./ledgerFakes.ts";
import {
  STOP_ATTEMPT_TIMEOUT_MS,
  STOP_COALESCE_MS,
  STOP_SWEEP_WINDOW_MS
} from "./runLedger.ts";
import { describeStop } from "./stopStatus.ts";

/** Ledger + daemon + native, client attached and synced with the daemon's runs. */
async function world(opts: { attach?: boolean; runs?: [string, "running" | "paused" | "queued"][] } = {}) {
  const daemon = new FakeDaemon();
  for (const [id, state] of opts.runs ?? []) {
    daemon.start(id, state);
  }
  const parts = makeLedger();
  const { ledger, native } = parts;
  ledger.onNativeSnapshot(native.snapshot());
  let handle = opts.attach === false ? null : ledger.attachClient(daemon.transport());
  if (handle) {
    ledger.applyList(ledger.beginList(handle)!, daemon.list());
  }
  /** Deliver queued native events to the ledger (the host's subscription). */
  const pumpNative = () => {
    for (const snap of native.events.splice(0)) {
      ledger.onNativeSnapshot(snap);
    }
  };
  const status = () =>
    describeStop({
      records: ledger.stopRecords(),
      runs: ledger.runEntries(),
      now: parts.clock.now(),
      connected: ledger.client() !== null
    });
  return {
    ...parts,
    daemon,
    get handle() {
      return handle;
    },
    attach() {
      handle = ledger.attachClient(daemon.transport());
      return handle;
    },
    pumpNative,
    status
  };
}

const UUID = /^[0-9a-f-]{36}$/;

test("one Stop is one decision: one id for the native token and the daemon stop_id", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  const rec = w.ledger.stopAll("banner");
  assert.match(rec.id, UUID);
  // Local input halts first, synchronously, with the Stop's id as token.
  assert.deepEqual(w.native.tokens, [rec.id]);
  assert.equal(w.daemon.requests.length, 1, "sent before any await");
  assert.deepEqual(w.daemon.requests[0]?.body, {
    stop_id: rec.id,
    scope: "all_existing",
    run_ids: ["r1"]
  });
  assert.equal(w.ledger.mayAct("r1"), false, "held at once");
  w.pumpNative();
  await flush();
  assert.equal(w.ledger.stopRecords().length, 1, "the token echo is not a second Stop");
});

test("retries reuse the SAME stop id (transport error, then a hung attempt) and never escalate", async () => {
  const w = await world({ runs: [["r1", "paused"]] });
  const rec = w.ledger.stopAll("hotkey");
  w.daemon.fail(0);
  await flush();
  assert.equal(w.status()?.kind, "unreachable");
  w.clock.advance(500);
  assert.equal(w.daemon.requests.length, 2);
  // the second attempt hangs: it is abandoned after the attempt timeout and retried
  w.clock.advance(STOP_ATTEMPT_TIMEOUT_MS);
  await flush();
  w.clock.advance(1000);
  assert.equal(w.daemon.requests.length, 3);
  for (const r of w.daemon.requests) {
    assert.equal(r.body.stop_id, rec.id);
    assert.deepEqual(r.body.run_ids, ["r1"]);
  }
  assert.equal(w.ledger.stopRecords().length, 1, "no new decision was made by the app");
  w.daemon.deliver(2);
  await flush();
  assert.equal(w.ledger.run("r1"), undefined, "terminal and pruned");
  assert.equal(w.status()?.kind, "stopped");
});

test("cancel_requested is shown as stopping, never stopped, until the run is terminal", async () => {
  const w = await world({ runs: [["live", "running"], ["idle", "paused"]] });
  const rec = w.ledger.stopAll("banner");
  w.pumpNative();
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.ledger.run("live")?.phase, "cancel_requested");
  assert.equal(w.ledger.mayAct("live"), false);
  const s = w.status();
  assert.equal(s?.kind, "stopping");
  assert.equal(s?.stopped, 1);
  assert.equal(s?.stopping, 1);
  assert.equal(w.native.acks.length, 0, "not acked while a targeted run is live");
  // the repeat with the same id is still "cancel_requested" while the tool call unwinds
  w.clock.advance(500);
  assert.equal(w.daemon.requests.at(-1)?.body.stop_id, rec.id);
  w.daemon.deliver();
  await flush();
  assert.equal(w.ledger.run("live")?.phase, "cancel_requested");
  // the run ends: run_finished confirms it
  w.daemon.end("live");
  w.ledger.observeRunFinished("live");
  assert.equal(w.status()?.kind, "stopped");
  assert.deepEqual(w.native.acks, [1], "acked once every targeted run is terminal");
});

test("a failed member stays visibly not stopped, never acts and is retried with the same id", async () => {
  const w = await world({ runs: [["r1", "paused"], ["r2", "paused"]] });
  w.daemon.failCancel.add("r2");
  const rec = w.ledger.stopAll("banner");
  w.pumpNative();
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.ledger.run("r2")?.phase, "failed");
  assert.equal(w.ledger.mayAct("r2"), false, "a failed cancel is never a re-track");
  w.ledger.observeRun("r2", "b1"); // another event for it changes nothing
  assert.equal(w.ledger.mayAct("r2"), false);
  const s = w.status();
  assert.equal(s?.kind, "partial");
  assert.equal(s?.tone, "danger");
  assert.equal(s?.failed, 1);
  assert.equal(s?.error, "cancel refused");
  w.daemon.failCancel.delete("r2");
  w.clock.advance(500);
  assert.equal(w.daemon.requests[1]?.body.stop_id, rec.id);
  w.daemon.deliver(1);
  await flush();
  assert.equal(w.status()?.kind, "stopped");
});

test("expired: shown as expired, retried ONLY with a new id, scope listed and the held runs", async () => {
  const w = await world({ runs: [["held", "running"]] });
  const first = w.ledger.stopAll("banner");
  w.pumpNative();
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.ledger.run("held")?.phase, "cancel_requested");
  // a run the user starts later
  w.daemon.start("later", "running");
  w.ledger.observeRun("later", "b1");
  w.daemon.forgetBook();
  w.clock.advance(500);
  w.daemon.deliver(1); // the repeat of `first`: expired
  await flush();
  const retry = w.daemon.requests[2];
  assert.ok(retry, "a successor request was sent");
  assert.notEqual(retry.body.stop_id, first.id, "new id");
  assert.equal(retry.body.scope, "listed", "never all_existing");
  assert.deepEqual(retry.body.run_ids, ["held"], "only what the app still holds");
  assert.equal(w.status()?.kind, "expired");
  assert.equal(w.status()?.stopId, first.id, "still the user's one decision");
  assert.equal(w.ledger.stopRecords().filter((r) => r.supersedes === null).length, 1);
  w.daemon.end("held");
  w.daemon.deliver(2);
  await flush();
  assert.equal(w.daemon.runs.get("later"), "running", "the later run was never targeted");
  assert.equal(w.ledger.mayAct("later"), true);
  assert.equal(w.status()?.kind, "stopped");
});

test("expired after the app learned T, nothing held: no request at all, the user is told", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  w.ledger.stopAll("banner");
  w.daemon.deliver(0); // T = {r1}, cancel_requested
  await flush();
  w.daemon.end("r1");
  w.ledger.observeRunFinished("r1");
  await flush();
  assert.equal(w.status()?.kind, "stopped");
});

test("expiry before the app ever learned T is never certified: unconfirmed, quarantine kept, no ack (r-94 #6)", async () => {
  const w = await world({ attach: false });
  const rec = w.ledger.stopAll("banner"); // no runs known, unsynced: all_existing
  w.pumpNative();
  w.daemon.start("R", "running");
  const h = w.attach();
  w.ledger.applyList(w.ledger.beginList(h)!, w.daemon.list()); // R seen while unanswered
  assert.equal(w.ledger.run("R")?.quarantinedBy, rec.id);
  w.daemon.processOnly(0); // the daemon fixes T = {R} and fires R's token ...
  w.daemon.requests[0]!.settled = true; // ... but the reply is lost
  w.daemon.requests[0]!.reject(new Error("reset"));
  await flush();
  w.daemon.forgetBook(); // TTL: the id is remembered, its set is not
  w.clock.advance(500);
  w.daemon.deliver(1); // expired
  await flush();
  assert.equal(w.daemon.runs.get("R"), "running", "R is still live at the daemon");
  assert.equal(w.ledger.stop(rec.id)?.phase, "unconfirmed");
  assert.deepEqual(w.native.acks, [], "unknown targets are never acknowledged");
  assert.equal(w.ledger.mayAct("R"), false, "R stays quarantined");
  const s = w.status();
  assert.equal(s?.kind, "unconfirmed");
  assert.equal(s?.quarantined, 1);
  assert.equal(w.daemon.requests.length, 2, "no automatic broader sweep");
  // a run first seen before the next list is quarantined too
  w.daemon.start("seen-late", "running");
  w.ledger.observeRun("seen-late", "b1");
  assert.equal(w.ledger.mayAct("seen-late"), false);
  // the user decides to let them continue: nothing is cancelled, acting resumes
  w.ledger.releaseUnconfirmed(rec.id);
  assert.equal(w.ledger.mayAct("R"), true);
  assert.equal(w.daemon.requests.length, 2);
  assert.deepEqual(w.native.acks, [1], "a user decision resolves it");
});

test("unconfirmed resolves to stopped only when a later list proves every candidate terminal", async () => {
  const w = await world({ attach: false });
  const rec = w.ledger.stopAll("banner");
  w.pumpNative();
  w.daemon.start("R", "running");
  const h = w.attach();
  w.ledger.applyList(w.ledger.beginList(h)!, w.daemon.list());
  w.daemon.fail(0);
  await flush();
  w.daemon.forgetBook();
  w.daemon.requests.length = 0;
  w.clock.advance(500);
  // the retry: the daemon remembers the id but not its set
  w.daemon.forgetBook();
  (w.daemon as unknown as { forgotten: Set<string> }).forgotten.add(rec.id);
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.ledger.stop(rec.id)?.phase, "unconfirmed");
  w.daemon.end("R");
  w.ledger.observeRunFinished("R");
  assert.equal(w.ledger.stop(rec.id)?.phase, "unconfirmed", "no list since the expiry yet");
  w.ledger.applyList(w.ledger.beginList(h)!, w.daemon.list());
  assert.equal(w.ledger.stop(rec.id)?.phase, "settled");
  assert.deepEqual(w.native.acks, [1]);
});

test("audited:false is surfaced on the status", async () => {
  const w = await world({ runs: [["r1", "paused"]] });
  w.daemon.audited = false;
  w.ledger.stopAll("banner");
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.status()?.audited, false);
  assert.equal(w.status()?.kind, "stopped");
});

test("a run started after the daemon fixed T is never stopped by that Stop, even on repeats", async () => {
  const w = await world({ runs: [["before", "running"]] });
  const rec = w.ledger.stopAll("hotkey");
  w.daemon.deliver(0);
  await flush();
  w.daemon.start("after", "queued");
  w.ledger.observeRun("after", "b1");
  w.clock.advance(500);
  w.daemon.deliver(1); // repeat, same id
  await flush();
  assert.equal(w.daemon.runs.get("after"), "queued");
  assert.equal(w.ledger.run("after")?.phase, "tracked");
  assert.equal(w.ledger.mayAct("after"), true);
  assert.ok(!w.ledger.stop(rec.id)?.members.includes("after"));
});

test("a run seen while a sweeping Stop is unanswered is quarantined, then freed if not in T", async () => {
  const w = await world({ runs: [["r1", "paused"]] });
  w.ledger.stopAll("banner");
  w.ledger.observeRun("new", "b1"); // created at the daemon after the Stop was processed
  assert.equal(w.ledger.mayAct("new"), false, "no action before the Stop's answer");
  w.daemon.deliver(0); // T fixed now; "new" does not exist at the daemon yet
  await flush();
  w.daemon.start("new", "running");
  assert.equal(w.ledger.mayAct("new"), true);
});

test("Stop with the daemon unreachable halts native locally and says daemon runs are unconfirmed", async () => {
  const w = await world({ attach: false });
  const rec = w.ledger.stopAll("hotkey");
  assert.deepEqual(w.native.tokens, [rec.id], "native halted without any network");
  const s = w.status();
  assert.equal(s?.kind, "unreachable");
  assert.equal(s?.localHalted, true);
  assert.equal(s?.dismissible, false);
  // The first client receives the Stop first, as all_existing (the app knew nothing).
  w.daemon.start("r1", "paused");
  const handle = w.attach();
  assert.equal(w.daemon.requests.length, 1);
  assert.equal(w.daemon.requests[0]?.body.scope, "all_existing");
  // restore: r1 is seen while the Stop is unanswered: quarantined
  w.ledger.applyList(w.ledger.beginList(handle)!, w.daemon.list());
  assert.equal(w.ledger.mayAct("r1"), false);
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.status()?.kind, "stopped");
});

test("reconnect: held runs stay held through a client swap, the same id goes to the new client", async () => {
  const w = await world({ runs: [["r1", "running"], ["r2", "paused"]] });
  assert.deepEqual(w.ledger.liveRunIds().sort(), ["r1", "r2"]);
  const old = w.handle;
  assert.ok(old);
  w.ledger.detachClient(old);
  const rec = w.ledger.stopAll("banner");
  assert.equal(w.daemon.requests.length, 0, "no client: queued");
  const next = w.attach();
  assert.equal(w.daemon.requests[0]?.body.stop_id, rec.id, "flushed first on the new client");
  // the restore list arrives before the Stop's answer: the runs stay held
  w.ledger.applyList(w.ledger.beginList(next)!, w.daemon.list());
  assert.equal(w.ledger.run("r1")?.phase, "held");
  assert.equal(w.ledger.run("r2")?.phase, "held");
  assert.equal(w.ledger.mayAct("r1"), false);
  w.daemon.deliver(0);
  await flush();
  assert.equal(w.ledger.run("r1")?.phase, "cancel_requested");
  assert.equal(w.ledger.run("r2")?.phase, "stopped", "cancelled (kept while r1 winds down)");
});

test("a list from a replaced client can only confirm terminal runs, never add live ones (N7)", async () => {
  const w = await world();
  const old = w.handle;
  assert.ok(old);
  const oldTicket = w.ledger.beginList(old);
  assert.ok(oldTicket);
  const next = w.attach();
  w.ledger.applyList(oldTicket, [{ id: "ghost", state: "running" }]);
  assert.equal(w.ledger.run("ghost"), undefined);
  assert.equal(next.synced, false, "the old list does not sync the new client");
  w.ledger.observeRun("r1", "b1");
  w.ledger.applyList(oldTicket, [{ id: "r1", state: "succeeded" }]);
  assert.equal(w.ledger.run("r1"), undefined, "terminal is still applied");
});

test("restore after reconnect: the daemon's open runs are live, a finished-during-list run is not", async () => {
  const w = await world({ attach: false });
  w.daemon.start("a", "running");
  w.daemon.start("b", "paused");
  w.daemon.start("done", "succeeded");
  const handle = w.attach();
  const ticket = w.ledger.beginList(handle);
  assert.ok(ticket);
  w.ledger.observeRunFinished("b"); // run_finished arrived while the list was in flight
  w.ledger.applyList(ticket, w.daemon.list());
  assert.deepEqual(w.ledger.liveRunIds(), ["a"]);
  assert.equal(w.ledger.run("a")?.processing, true);
  assert.equal(handle.synced, true);
});

test("ack_stop is sent only once every targeted run is terminal", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  w.ledger.stopAll("hotkey");
  w.pumpNative();
  w.daemon.deliver(0);
  await flush();
  assert.deepEqual(w.native.acks, []);
  // later native snapshots (step text, hotkey apply) re-evaluate the ack: still none
  w.native.touch();
  w.pumpNative();
  w.clock.advance(500);
  w.daemon.deliver();
  await flush();
  assert.deepEqual(w.native.acks, [], "r1 is cancel_requested, not terminal");
  w.ledger.observeRunFinished("r1");
  await flush();
  assert.deepEqual(w.native.acks, [1]);
});

test("own token echo is no second Stop; an untokened stopSeq rise (widget, tray, global hotkey) is one", async () => {
  const w = await world({ runs: [["r1", "paused"]] });
  const ui = w.ledger.stopAll("banner");
  w.pumpNative();
  w.daemon.deliver(0);
  await flush();
  assert.deepEqual(w.ledger.stop(ui.id)?.nativeSeqs, [1]);
  w.clock.advance(STOP_COALESCE_MS + 1000);
  w.daemon.start("r2", "paused");
  w.ledger.observeRun("r2", "b1");
  w.native.userStop(null); // widget button
  w.pumpNative();
  const roots = w.ledger.stopRecords().filter((r) => r.kind === "all" && r.supersedes === null);
  assert.equal(roots.length, 2);
  const nativeRec = roots[1];
  assert.equal(nativeRec?.origin, "native");
  assert.deepEqual(nativeRec?.nativeSeqs, [2]);
  assert.deepEqual(w.daemon.requests.at(-1)?.body.run_ids, ["r2"]);
});

test("widget and main pressing Stop for the same moment are one decision; later presses are new ones", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  w.native.holdReplies = true;
  const main = w.ledger.stopAll("hotkey"); // in-window hotkey (token)
  w.native.userStop(null); // global listener / widget on the same press
  w.pumpNative();
  w.native.releaseReplies();
  await flush();
  assert.equal(w.ledger.stopRecords().length, 1);
  assert.deepEqual(w.ledger.stop(main.id)?.nativeSeqs.sort(), [1, 2]);
  assert.equal(w.daemon.requests.length, 1);
  w.daemon.deliver(0);
  await flush();
  // widget first, then the main banner within the window: still one decision
  w.clock.advance(5000);
  w.native.userStop(null);
  w.pumpNative();
  const before = w.ledger.stopRecords().length;
  w.clock.advance(STOP_COALESCE_MS - 50);
  w.ledger.stopAll("banner");
  assert.equal(w.ledger.stopRecords().length, before, "joined the widget's Stop");
  // a deliberate second press later is a second Stop that re-sends unresolved members
  w.clock.advance(1000);
  const sentBefore = w.daemon.requests.length;
  w.ledger.stopAll("banner");
  assert.ok(w.daemon.requests.length >= sentBefore + 2, "new Stop + immediate retry of the open one");
});

test("a normal finish (no stopSeq rise) and other native changes never create a Stop", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  w.native.mode = "bot_driving";
  w.native.finish(w.native.runGeneration);
  for (let i = 0; i < 1000; i += 1) {
    w.native.touch(); // step text, hotkey apply, refused actions
  }
  w.pumpNative();
  assert.equal(w.ledger.stopRecords().length, 0);
  assert.equal(w.ledger.mayAct("r1"), true);
  // ... and a thousand such events cannot evict a Stop that happens afterwards
  w.native.userStop(null);
  for (let i = 0; i < 1000; i += 1) {
    w.native.touch();
  }
  w.pumpNative();
  assert.equal(w.ledger.stopRecords().length, 1);
});

test("a failed native read (no stopSeq) never seeds the stop cursor", () => {
  const { ledger } = makeLedger();
  ledger.onNativeSnapshot({}); // getControlState() fallback
  ledger.onNativeSnapshot({ stopSeq: 4, stopAckedSeq: 4, stateRev: 9 });
  assert.equal(ledger.stopRecords().length, 0, "seq 4 was already acknowledged");
});

test("a Stop nobody handled before the ledger listened (stopSeq > stopAckedSeq at start) is honoured", () => {
  const { ledger, native } = makeLedger();
  native.userStop(null); // pressed while the webview was loading
  ledger.onNativeSnapshot(native.snapshot());
  const recs = ledger.stopRecords();
  assert.equal(recs.length, 1);
  assert.deepEqual(recs[0]?.nativeSeqs, [1]);
});

test("late first delivery: listed when the app was synced, still all_existing when it was not", async () => {
  const synced = await world({ runs: [["r1", "paused"]] });
  const a = synced.ledger.stopAll("banner");
  // every attempt fails until the sweep window has passed
  while (synced.clock.now() <= STOP_SWEEP_WINDOW_MS) {
    for (const open of synced.daemon.open()) {
      synced.daemon.fail(synced.daemon.requests.indexOf(open));
    }
    await flush();
    synced.clock.advance(500);
  }
  assert.equal(synced.daemon.requests[0]?.body.scope, "all_existing", "inside the window");
  synced.clock.advance(2000); // the next retry is after the window
  const retry = synced.daemon.requests.at(-1);
  assert.equal(retry?.body.stop_id, a.id);
  assert.equal(retry?.body.scope, "listed");
  assert.deepEqual(retry?.body.run_ids, ["r1"]);

  const unsynced = await world({ attach: false });
  const b = unsynced.ledger.stopAll("banner");
  unsynced.clock.advance(30_000);
  unsynced.attach();
  assert.equal(unsynced.daemon.requests[0]?.body.stop_id, b.id);
  assert.equal(unsynced.daemon.requests[0]?.body.scope, "all_existing");
});

test("webview reload: an unacknowledged Stop is restored, not duplicated, and keeps its id", async () => {
  const storage = new MemoryStorage();
  const clock = new FakeClock();
  const native = new FakeNative();
  const daemon = new FakeDaemon();
  daemon.start("r1", "running");
  const first = makeLedger({ clock, native, storage, session: "s" });
  first.ledger.onNativeSnapshot(native.snapshot());
  const h = first.ledger.attachClient(daemon.transport());
  first.ledger.applyList(first.ledger.beginList(h)!, daemon.list());
  const rec = first.ledger.stopAll("banner");
  first.ledger.onNativeSnapshot(native.events.at(-1) ?? {});
  daemon.deliver(0);
  await flush();
  assert.equal(native.stopAckedSeq, 0, "r1 is still winding down");

  // reload: same webview session, native state survives
  const second = makeLedger({ clock: new FakeClock(), native, storage, session: "s" });
  second.ledger.onNativeSnapshot(native.snapshot());
  assert.equal(second.ledger.stopRecords().length, 1, "seq 1 is claimed by the restored record");
  assert.equal(second.ledger.mayAct("r1"), false);
  const h2 = second.ledger.attachClient(daemon.transport());
  assert.equal(daemon.requests.at(-1)?.body.stop_id, rec.id, "same id after the reload");
  second.ledger.applyList(second.ledger.beginList(h2)!, daemon.list());
  assert.equal(second.ledger.run("r1")?.phase, "cancel_requested", "never back to tracked");
  daemon.end("r1");
  second.ledger.observeRunFinished("r1");
  daemon.deliver();
  await flush();
  assert.deepEqual(native.acks, [1]);
});

test("app restart: a Stop that never reached the daemon is narrowed to the runs it knew, or asks again", () => {
  const storage = new MemoryStorage();
  const before = makeLedger({ storage, session: "old" });
  const h = before.ledger.attachClient({ stopRuns: () => new Promise(() => {}) });
  before.ledger.applyList(before.ledger.beginList(h)!, [{ id: "r1", state: "running" }]);
  before.ledger.detachClient(h);
  before.ledger.stopAll("banner");

  const daemon = new FakeDaemon();
  daemon.start("r1", "running");
  daemon.start("started-after-restart", "running");
  const after = makeLedger({ storage, session: "new" });
  after.ledger.onNativeSnapshot(after.native.snapshot());
  after.ledger.attachClient(daemon.transport());
  assert.equal(daemon.requests[0]?.body.scope, "listed", "never a sweep after a restart");
  assert.deepEqual(daemon.requests[0]?.body.run_ids, ["r1"]);

  const empty = new MemoryStorage();
  const lone = makeLedger({ storage: empty, session: "old" });
  lone.ledger.stopAll("banner"); // no client, no runs known
  const restarted = makeLedger({ storage: empty, session: "new" });
  const status = describeStop({
    records: restarted.ledger.stopRecords(),
    runs: restarted.ledger.runEntries(),
    now: 0,
    connected: false
  });
  assert.equal(status?.kind, "unconfirmable");
});

test("cancelOne (chat) holds just that run and sends a listed Stop of its own", async () => {
  const w = await world({ runs: [["r1", "running"], ["r2", "running"]] });
  const rec = w.ledger.cancelOne("r1");
  assert.equal(rec.kind, "single");
  assert.deepEqual(w.daemon.requests[0]?.body, { stop_id: rec.id, scope: "listed", run_ids: ["r1"] });
  assert.deepEqual(w.native.tokens, [], "no native Stop for a single run");
  assert.equal(w.ledger.mayAct("r1"), false);
  assert.equal(w.ledger.mayAct("r2"), true);
  assert.equal(w.status(), null, "not a global Stop");
});

test("a second Stop of a held run is a hard stop: its own decision with force", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  const first = w.ledger.cancelOne("r1");
  const hard = w.ledger.cancelOne("r1", { force: true });
  assert.notEqual(hard.id, first.id);
  assert.deepEqual(w.daemon.requests[1]?.body, { stop_id: hard.id, scope: "listed", run_ids: ["r1"], force: true });
  assert.equal("force" in (w.daemon.requests[0]?.body ?? {}), false, "the first press is not forced");
});

test("control_requested ids are answered once across bridges (replay after reconnect)", () => {
  const { ledger } = makeLedger();
  assert.equal(ledger.claimRequest("req-1"), true);
  assert.equal(ledger.claimRequest("req-1"), false);
});

test("an older restore list never reconciles a run seen after it was issued (r-94 #3)", async () => {
  const w = await world({ attach: false });
  const h = w.attach();
  const a = w.ledger.beginList(h)!; // list A: captures an empty daemon
  const listA = w.daemon.list();
  w.daemon.start("late", "running");
  const b = w.ledger.beginList(h)!; // list B sees the run
  w.ledger.applyList(b, w.daemon.list());
  w.ledger.stopAll("hotkey");
  w.pumpNative();
  w.daemon.deliver(0); // cancel_requested
  await flush();
  assert.equal(w.ledger.run("late")?.phase, "cancel_requested");
  w.ledger.applyList(a, listA); // A finally arrives, empty
  assert.equal(w.ledger.run("late")?.phase, "cancel_requested", "not marked terminal by A");
  assert.notEqual(w.status()?.kind, "stopped");
  assert.deepEqual(w.native.acks, []);
  // even a NEWER list issued before the run was first seen cannot drop it
  const c = w.ledger.beginList(h)!;
  w.ledger.observeRun("later", "b1");
  w.ledger.applyList(c, w.daemon.list().filter((r) => r.id !== "later"));
  assert.equal(w.ledger.run("later")?.phase, "tracked");
});

test("native: a Stop made before the first snapshot is bound by its token, never duplicated (r-94 #4a)", async () => {
  const daemon = new FakeDaemon();
  const { ledger, native, clock } = makeLedger();
  const h = ledger.attachClient(daemon.transport());
  ledger.applyList(ledger.beginList(h)!, []);
  native.deferCommands = true; // the native command runs later
  const s = ledger.stopAll("banner"); // before any native snapshot was routed
  daemon.deliver(0);
  await flush();
  daemon.start("after", "running");
  ledger.observeRun("after", "b1");
  clock.advance(1000); // well outside the coalesce window
  native.runCommands();
  // the first snapshot routed carries S's token (its event / reply)
  for (const snap of native.events.splice(0)) {
    ledger.onNativeSnapshot(snap);
  }
  await flush();
  assert.equal(ledger.stopRecords().length, 1, "no second decision");
  assert.deepEqual(ledger.stop(s.id)?.nativeSeqs, [1]);
  assert.equal(daemon.requests.length, 1, "nothing re-sent");
  assert.equal(ledger.mayAct("after"), true);
});

test("native: an independent untokened Stop is a new decision even while our own invoke is pending (r-94 #4b)", async () => {
  const w = await world({ runs: [["r1", "paused"]] });
  w.native.deferCommands = true; // S's stop_bot_run has not executed yet
  const s = w.ledger.stopAll("banner");
  w.daemon.deliver(0);
  await flush();
  w.daemon.start("later", "running");
  w.ledger.observeRun("later", "b1");
  w.clock.advance(1000); // an independent press, outside the coalesce window
  w.native.userStop(null); // global hotkey: seq 1, no token
  w.pumpNative();
  w.native.runCommands(); // S executes now: seq 2, token S
  w.pumpNative();
  await flush();
  const roots = w.ledger.stopRecords().filter((r) => r.kind === "all");
  assert.equal(roots.length, 2, "two decisions, two ids");
  assert.deepEqual(w.ledger.stop(s.id)?.nativeSeqs, [2]);
  const other = roots.find((r) => r.id !== s.id);
  assert.deepEqual(other?.nativeSeqs, [1]);
  assert.equal(w.ledger.mayAct("later"), false, "the second Stop holds the later run");
  assert.ok(w.daemon.requests.some((r) => r.body.stop_id === other?.id));
});

test("native: the in-window hotkey and the global listener on one press stay one decision after the reply (r-94 #4c)", async () => {
  const w = await world({ runs: [["r1", "paused"]] });
  const s = w.ledger.stopAll("hotkey");
  w.daemon.deliver(0); // the daemon answers before the global listener's event arrives
  await flush();
  w.clock.advance(100);
  w.native.userStop(null);
  w.pumpNative();
  assert.equal(w.ledger.stopRecords().length, 1);
  assert.equal(w.ledger.stop(s.id)?.nativeSeqs.length, 2);
});

test("after the daemon told the app T, a restarted daemon never re-sweeps: retries are listed T (r-94 #5)", async () => {
  const w = await world({ runs: [["r1", "running"]] });
  const s = w.ledger.stopAll("banner");
  w.daemon.deliver(0); // T = {r1}, cancel_requested
  await flush();
  w.daemon.restart(); // fresh process: no book, no tombstones
  w.daemon.start("phone-run", "running");
  const h = w.attach(); // the client reconnects and flushes the open Stop first
  const retry = w.daemon.requests.at(-1);
  assert.equal(retry?.body.stop_id, s.id, "same decision, same id");
  assert.equal(retry?.body.scope, "listed");
  assert.deepEqual(retry?.body.run_ids, ["r1"]);
  w.daemon.deliver();
  await flush();
  assert.equal(w.daemon.runs.get("phone-run"), "running", "the later run was not cancelled");
  w.ledger.applyList(w.ledger.beginList(h)!, w.daemon.list());
  assert.equal(w.ledger.mayAct("phone-run"), true);
});

test("a list older than one already applied adds nothing: a run that ended in between never becomes a ghost (r-94 #3)", async () => {
  const w = await world({ attach: false });
  w.daemon.start("ghost", "running");
  const h = w.attach();
  const older = w.ledger.beginList(h)!;
  const olderRows = w.daemon.list(); // still shows "ghost" open
  w.daemon.runs.delete("ghost"); // it ended; no event reached us
  const newer = w.ledger.beginList(h)!;
  w.ledger.applyList(newer, w.daemon.list());
  w.ledger.applyList(older, olderRows); // arrives last
  assert.equal(w.ledger.run("ghost"), undefined, "a dead run must not be tracked (or driven)");
});
