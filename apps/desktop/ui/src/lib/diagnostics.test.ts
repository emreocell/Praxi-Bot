import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError } from "../api/client.ts";
import { MockApiClient } from "../api/mock.ts";
import type { DoctorCheck } from "../api/types.ts";
import {
  DiagnosticsQueue,
  INITIAL_DIAGNOSTICS,
  checkTone,
  controls,
  diagnosticsReducer,
  heading,
  repairButton,
  sortChecks,
  summarize
} from "./diagnostics.ts";

function check(overrides: Partial<DoctorCheck> = {}): DoctorCheck {
  return {
    id: "data_dir",
    title: "data dir",
    status: "ok",
    detail: "fine",
    hint: null,
    repair: null,
    ...overrides
  };
}

test("status renders as a tone; skipped is neutral, never an error", () => {
  assert.equal(checkTone("ok"), "ok");
  assert.equal(checkTone("warn"), "warn");
  assert.equal(checkTone("fail"), "fail");
  assert.equal(checkTone("skipped"), "neutral");
  const s = summarize([
    check({ status: "skipped" }),
    check({ status: "skipped" }),
    check({ status: "ok" })
  ]);
  assert.deepEqual(s, { fail: 0, warn: 0, ok: 1, neutral: 2, overall: "ok" });
  assert.equal(summarize([check({ status: "warn" }), check({ status: "fail" })]).overall, "fail");
});

test("problems sort first, otherwise the daemon order is kept", () => {
  const sorted = sortChecks([
    check({ id: "a", status: "ok" }),
    check({ id: "b", status: "skipped" }),
    check({ id: "c", status: "warn" }),
    check({ id: "d", status: "ok" }),
    check({ id: "e", status: "fail" })
  ]);
  assert.deepEqual(sorted.map((c) => c.id), ["e", "c", "a", "d", "b"]);
});

test("a repair button only appears for a fixable warning or failure", () => {
  const idle = { phase: "ready" as const, running: null };
  assert.equal(repairButton(check({ status: "fail" }), idle), null);
  assert.equal(repairButton(check({ status: "ok", repair: "create_data_dir" }), idle), null);
  assert.equal(repairButton(check({ status: "skipped", repair: "create_data_dir" }), idle), null);
  assert.deepEqual(repairButton(check({ status: "warn", repair: "create_data_dir" }), idle), {
    repair: "create_data_dir",
    disabled: false,
    busy: false
  });
});

test("the install-browser repair is a normal repair button and shows as busy while it runs", () => {
  const idle = { phase: "ready" as const, running: null };
  const missing = check({ status: "warn", repair: "install_browser" });
  assert.deepEqual(repairButton(missing, idle), { repair: "install_browser", disabled: false, busy: false });
  const running = { phase: "ready" as const, running: "install_browser" as const };
  assert.deepEqual(repairButton(missing, running), { repair: "install_browser", disabled: true, busy: true });
});

test("repair buttons are disabled while a repair runs or checks reload", () => {
  const fixable = check({ status: "fail", repair: "reload_providers" });
  const other = check({ id: "data_dir", status: "warn", repair: "create_data_dir" });
  let state = diagnosticsReducer(INITIAL_DIAGNOSTICS, { type: "load_ok", checks: [fixable, other] });
  state = diagnosticsReducer(state, { type: "repair_start", repair: "reload_providers" });
  assert.deepEqual(repairButton(fixable, state), { repair: "reload_providers", disabled: true, busy: true });
  assert.deepEqual(repairButton(other, state), { repair: "create_data_dir", disabled: true, busy: false });
  // A second click while one runs is ignored.
  const again = diagnosticsReducer(state, { type: "repair_start", repair: "create_data_dir" });
  assert.equal(again.running, "reload_providers");
  state = diagnosticsReducer(state, {
    type: "repair_done",
    checkId: fixable.id,
    outcome: { ok: true, text: "1 provider(s) loaded" }
  });
  assert.equal(state.running, null);
  assert.equal(repairButton(other, state)?.disabled, false);
  assert.deepEqual(state.outcomes[fixable.id], { ok: true, text: "1 provider(s) loaded" });
  const loading = diagnosticsReducer(state, { type: "load_start" });
  assert.equal(repairButton(other, loading)?.disabled, true);
});

test("load errors are kept until the next successful load", () => {
  let state = diagnosticsReducer(INITIAL_DIAGNOSTICS, { type: "load_start" });
  assert.equal(state.phase, "loading");
  state = diagnosticsReducer(state, { type: "load_error", error: "offline" });
  assert.equal(state.phase, "error");
  assert.equal(state.error, "offline");
  state = diagnosticsReducer(state, { type: "load_ok", checks: [check()] });
  assert.equal(state.error, null);
  assert.equal(state.phase, "ready");
});

test("the client talks to the doctor routes; the mock fixes only allow-listed repairs", async () => {
  const client = new MockApiClient();
  const report = await client.doctor();
  assert.equal(report.ok, false);
  const fixable = report.checks.find((c) => c.repair === "reload_providers");
  assert.ok(fixable);
  const result = await client.doctorRepair("reload_providers");
  assert.equal(result.repair, "reload_providers");
  assert.deepEqual(client.repairs, ["reload_providers"]);
  const after = await client.doctor();
  assert.equal(after.checks.find((c) => c.id === fixable.id)?.status, "ok");
  await assert.rejects(
    client.doctorRepair("regenerate_api_token" as never),
    (e: unknown) => e instanceof ApiError && e.status === 404
  );
  const md = await client.doctorReport();
  assert.ok(md.startsWith("# Praxi diagnostics report"));
});

/** A promise resolved from outside. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/** A daemon with one diagnostics slot: an overlapping call gets 429. */
function slotDaemon() {
  let inFlight = 0;
  const calls: string[] = [];
  const pending: Array<{ name: string; release: () => void }> = [];
  const call = async (name: string) => {
    calls.push(name);
    if (inFlight > 0) {
      throw new ApiError(429, "too_many_requests", "diagnostics are already running");
    }
    inFlight += 1;
    const d = deferred<void>();
    pending.push({ name, release: () => d.resolve() });
    await d.promise;
    inFlight -= 1;
    return name;
  };
  return { call, calls, pending };
}

test("copy report and run again never overlap on the daemon's single slot", async () => {
  const daemon = slotDaemon();
  const busyLog: Array<string | null> = [];
  const queue = new DiagnosticsQueue((b) => busyLog.push(b));
  // The automatic check run starts when the card opens.
  const load = queue.run("load", () => daemon.call("doctor"));
  assert.equal(queue.current, "load");
  // Copy and Run again are disabled while it runs...
  assert.deepEqual(controls(true, queue.current), {
    rerunDisabled: true,
    copyDisabled: true,
    copying: false,
    loading: true
  });
  // ...and a click that still gets through never reaches the daemon.
  assert.deepEqual(await queue.run("copy", () => daemon.call("report")), { started: false });
  assert.deepEqual(daemon.calls, ["doctor"]);
  daemon.pending[0].release();
  assert.deepEqual(await load, { started: true, value: "doctor" });

  // Copy now runs; Run again and repairs wait for it.
  const copy = queue.run("copy", () => daemon.call("report"));
  assert.deepEqual(controls(true, queue.current), {
    rerunDisabled: true,
    copyDisabled: true,
    copying: true,
    loading: false
  });
  const fix = check({ status: "fail", repair: "create_data_dir" });
  assert.equal(repairButton(fix, { phase: "ready", running: null }, queue.current)?.disabled, true);
  assert.deepEqual(await queue.run("load", () => daemon.call("doctor")), { started: false });
  daemon.pending[1].release();
  assert.deepEqual(await copy, { started: true, value: "report" });
  assert.deepEqual(daemon.calls, ["doctor", "report"], "no 429 was provoked");
  assert.deepEqual(busyLog, ["load", null, "copy", null]);
  assert.deepEqual(controls(true, queue.current), {
    rerunDisabled: false,
    copyDisabled: false,
    copying: false,
    loading: false
  });
  assert.equal(repairButton(fix, { phase: "ready", running: null }, queue.current)?.disabled, false);
});

test("a failed task frees the slot", async () => {
  const queue = new DiagnosticsQueue();
  await assert.rejects(queue.run("copy", () => Promise.reject(new Error("boom"))));
  assert.equal(queue.current, null);
  assert.deepEqual(await queue.run("load", async () => 1), { started: true, value: 1 });
});

test("the heading follows the phase: error and empty never say running", () => {
  const s = (phase: "idle" | "loading" | "ready" | "error", checks: DoctorCheck[] = []) => ({ phase, checks });
  assert.equal(heading(false, s("idle"), 0), "title");
  assert.equal(heading(true, s("loading"), 0), "running");
  assert.equal(heading(true, s("idle"), 0), "running");
  assert.equal(heading(true, s("error"), 0), "error");
  assert.equal(heading(true, s("ready"), 0), "empty");
  assert.equal(heading(true, s("ready", [check()]), 0), "allGood");
  assert.equal(heading(true, s("error", [check({ status: "fail" })]), 1), "problems");
});
