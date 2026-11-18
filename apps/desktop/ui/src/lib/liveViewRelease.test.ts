import assert from "node:assert/strict";
import { test } from "node:test";
import type { ControlStateDto, ReleaseRequest } from "../api/types.ts";
import { humanLine } from "./humanError.ts";
import { runClearPause, runRelease } from "./liveViewRelease.ts";

const NIL = "00000000-0000-0000-0000-000000000000";

type Take = { id: string; state: string; owner?: string; target_id?: string; reason?: string | null };

const dto = (takeover: Take | null, latched?: string): ControlStateDto =>
  ({
    app_connected: true,
    expires_in_ms: 0,
    latched,
    takeover: takeover && { owner: "me", target_id: "t1", control_epoch: 1, session_id: "s", input_expires_in_ms: 0, ...takeover }
  }) as unknown as ControlStateDto;

const req = { takeover_id: "to-1", request_id: "r1", control_epoch: 1, outcome: "done" } as unknown as ReleaseRequest;

test("a release whose response no longer shows the takeover is confirmed without a re-read", async () => {
  const resp = dto(null);
  let reads = 0;
  const out = await runRelease(
    { release: async () => resp, getControlState: async () => (reads++, resp) },
    req
  );
  assert.deepEqual(out, { kind: "confirmed", dto: resp });
  assert.equal(reads, 0);
});

test("a rejected POST is confirmed when fresh state shows the takeover gone", async () => {
  const fresh = dto({ id: "other", state: "active" });
  const out = await runRelease(
    { release: () => Promise.reject(new Error("lost")), getControlState: async () => fresh },
    req
  );
  assert.deepEqual(out, { kind: "confirmed", dto: fresh });
});

test("a released takeover counts as confirmed", async () => {
  const resp = dto({ id: "to-1", state: "released" });
  const out = await runRelease({ release: async () => resp, getControlState: async () => resp }, req);
  assert.equal(out.kind, "confirmed");
});

test("a rejected POST with the takeover still there fails with the error code and fresh state", async () => {
  const err = Object.assign(new Error("nope"), { code: "forbidden" });
  const fresh = dto({ id: "to-1", state: "active" });
  const out = await runRelease(
    { release: () => Promise.reject(err), getControlState: async () => fresh },
    req
  );
  assert.deepEqual(out, { kind: "failed", code: "forbidden", message: humanLine(err), dto: fresh });
});

test("an error without a code falls back to release_failed; unreadable state keeps dto null", async () => {
  const out = await runRelease(
    { release: () => Promise.reject(new Error("x")), getControlState: () => Promise.reject(new Error("y")) },
    req
  );
  assert.equal(out.kind, "failed");
  assert.equal(out.kind === "failed" && out.code, "release_failed");
  assert.equal(out.kind === "failed" && out.dto, null);
});

test("a POST that succeeds but is not confirmed is release_unconfirmed", async () => {
  const still = dto({ id: "to-1", state: "active" });
  const out = await runRelease({ release: async () => still, getControlState: async () => still }, req);
  assert.deepEqual(out, {
    kind: "failed",
    code: "release_unconfirmed",
    message: "release not confirmed by daemon state",
    dto: still
  });
});

const noop = async (): Promise<ControlStateDto> => {
  throw new Error("must not be called");
};

test("clearing with no target aborts without touching the daemon", async () => {
  const out = await runClearPause({ getControlState: noop, recover: noop, releaseLatch: noop }, null);
  assert.deepEqual(out, { kind: "aborted", fresh: null });
});

test("an own paused takeover is recovered only while it is still the same one", async () => {
  const paused = dto({ id: "to-1", state: "paused" });
  const after = dto(null);
  const out = await runClearPause(
    { getControlState: async () => paused, recover: async () => after, releaseLatch: noop },
    { kind: "own", takeoverId: "to-1" }
  );
  assert.deepEqual(out, { kind: "cleared", dto: after });
  const changed = dto({ id: "to-2", state: "paused" });
  const aborted = await runClearPause(
    { getControlState: async () => changed, recover: noop, releaseLatch: noop },
    { kind: "own", takeoverId: "to-1" }
  );
  assert.deepEqual(aborted, { kind: "aborted", fresh: changed });
});

test("a foreign paused takeover needs the same owner, target and reason", async () => {
  const expected = { owner: "peer", targetId: "t1", reason: null };
  const same = dto({ id: NIL, state: "paused", owner: "peer", target_id: "t1" });
  const cleared = dto(null);
  const ok = await runClearPause(
    { getControlState: async () => same, recover: async () => cleared, releaseLatch: noop },
    { kind: "foreign", expected }
  );
  assert.equal(ok.kind, "cleared");
  const other = dto({ id: NIL, state: "paused", owner: "someone-else", target_id: "t1" });
  const no = await runClearPause(
    { getControlState: async () => other, recover: noop, releaseLatch: noop },
    { kind: "foreign", expected }
  );
  assert.equal(no.kind, "aborted");
});

test("a plain latch is released the ordinary way", async () => {
  const state = dto(null, "paused");
  const after = dto(null);
  const out = await runClearPause(
    { getControlState: async () => state, recover: noop, releaseLatch: async () => after },
    { kind: "latch" }
  );
  assert.deepEqual(out, { kind: "cleared", dto: after });
});

test("a latch target aborts when a takeover appeared meanwhile", async () => {
  const state = dto({ id: "x", state: "active" }, "paused");
  const out = await runClearPause(
    { getControlState: async () => state, recover: noop, releaseLatch: noop },
    { kind: "latch" }
  );
  assert.deepEqual(out, { kind: "aborted", fresh: state });
});

test("a refused latch release falls back to recover only while the latch is still paused with no takeover", async () => {
  const paused = dto(null, "paused");
  const after = dto(null);
  const refuse = () => Promise.reject(new Error("refused"));
  const ok = await runClearPause(
    { getControlState: async () => paused, recover: async () => after, releaseLatch: refuse },
    { kind: "latch" }
  );
  assert.deepEqual(ok, { kind: "cleared", dto: after });

  let call = 0;
  const raced = dto({ id: "peer", state: "active" }, "paused");
  const aborted = await runClearPause(
    { getControlState: async () => (call++ === 0 ? paused : raced), recover: noop, releaseLatch: refuse },
    { kind: "latch" }
  );
  assert.deepEqual(aborted, { kind: "aborted", fresh: raced });
});

test("unreadable state and a failing recover both fail closed", async () => {
  const unreadable = await runClearPause(
    { getControlState: () => Promise.reject(new Error("down")), recover: noop, releaseLatch: noop },
    { kind: "own", takeoverId: "to-1" }
  );
  assert.equal(unreadable.kind, "failed");
  assert.equal(unreadable.kind === "failed" && unreadable.code, "recover_failed");

  const paused = dto({ id: "to-1", state: "paused" });
  const failed = await runClearPause(
    {
      getControlState: async () => paused,
      recover: () => Promise.reject(Object.assign(new Error("e"), { code: "busy" })),
      releaseLatch: noop
    },
    { kind: "own", takeoverId: "to-1" }
  );
  assert.equal(failed.kind === "failed" && failed.code, "busy");
});
