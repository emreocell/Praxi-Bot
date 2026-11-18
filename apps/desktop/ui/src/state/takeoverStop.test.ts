import assert from "node:assert/strict";
import { test } from "node:test";
import type { ControlStateDto } from "../api/types.ts";
import { createInitialLiveViewState, shouldOverrideOnStop } from "../lib/liveViewState.ts";
import { FakeClock, flush } from "./ledgerFakes.ts";
import { OVERRIDE_ATTEMPT_MS, TakeoverStop } from "./takeoverStop.ts";

type Client = { name: string };

function dto(state: "active" | "acquiring" | "paused" | null, owner = "local-ui:1"): ControlStateDto {
  return {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: state === "paused" ? "paused" : null,
    mode: "idle",
    release: null,
    takeover: state
      ? {
          id: "to-1",
          control_epoch: 1,
          input_expires_in_ms: 10_000,
          owner,
          session_id: "s",
          state,
          target_id: "t"
        }
      : null
  };
}

/** The same deps LiveViewContext gives TakeoverStop, with a fake client boundary. */
function harness() {
  const clock = new FakeClock();
  let current: Client | null = { name: "A" };
  let snapshotFor: Client | null = null;
  let snapshot: ControlStateDto | null = null;
  const posts: { client: string; resolve: (d: ControlStateDto) => void; reject: (e: Error) => void; signal: AbortSignal }[] = [];
  const applied: { client: string; dto: ControlStateDto }[] = [];
  const gate = new TakeoverStop<Client>({
    client: () => current,
    controlled: () =>
      current !== null && snapshotFor === current && snapshot !== null
        ? shouldOverrideOnStop(createInitialLiveViewState(null), snapshot)
        : null,
    post: (c, signal) =>
      new Promise((resolve, reject) => {
        posts.push({ client: c.name, resolve, reject, signal });
        signal.addEventListener("abort", () => reject(new Error("aborted")));
      }),
    apply: (c, d) => {
      applied.push({ client: c.name, dto: d });
      snapshot = d;
      snapshotFor = c;
    },
    schedule: (fn, ms) => clock.set(fn, ms),
    cancel: (handle) => clock.clear(handle)
  });
  return {
    gate,
    clock,
    posts,
    applied,
    /** A control snapshot read from the current client (poll / refresh). */
    snapshot(d: ControlStateDto) {
      snapshot = d;
      snapshotFor = current;
      gate.pump();
    },
    swap(next: Client | null) {
      current = next;
      snapshotFor = null; // the old client's snapshot says nothing about the new one
      gate.pump();
    }
  };
}

test("Stop halts takeover input synchronously, before any daemon answer", () => {
  const h = harness();
  h.snapshot(dto("active"));
  assert.equal(h.gate.inputAllowed(), true);
  h.gate.stop(false);
  assert.equal(h.gate.inputAllowed(), false, "no further pointer / key event, no lease renewal");
  assert.equal(h.posts.length, 1);
  h.gate.resume(); // the user takes control again (a new decision)
  assert.equal(h.gate.inputAllowed(), true);
});

test("acquiring and foreign active takeovers are overridden; paused or none are not", async () => {
  for (const [state, owner, expected] of [
    ["acquiring", "local-ui:1", 1],
    ["active", "peer:phone", 1],
    ["paused", "local-ui:1", 0],
    [null, "local-ui:1", 0]
  ] as const) {
    const h = harness();
    h.snapshot(dto(state, owner));
    h.gate.stop(false);
    assert.equal(h.posts.length, expected, `${state} by ${owner}`);
    assert.equal(h.gate.inputAllowed(), false);
  }
});

test("no client at Stop: the override is retained and sent once a client and its snapshot exist", () => {
  const h = harness();
  h.swap(null);
  h.gate.stop(false);
  assert.equal(h.posts.length, 0);
  assert.equal(h.gate.owes(), true);
  h.swap({ name: "B" });
  assert.equal(h.posts.length, 0, "unknown until B's snapshot: not assumed uncontrolled");
  h.snapshot(dto("active"));
  assert.equal(h.posts.length, 1);
  assert.equal(h.posts[0]?.client, "B");
});

test("a failed override stays owed and is retried by the next snapshot (no retry timer)", async () => {
  const h = harness();
  h.snapshot(dto("active"));
  h.gate.stop(false);
  h.posts[0]?.reject(new Error("500"));
  await flush();
  assert.equal(h.gate.owes(), true);
  assert.equal(h.clock.pending(), 0, "no timer is left behind");
  h.snapshot(dto("active")); // the 5 s control poll
  assert.equal(h.posts.length, 2);
  h.posts[1]?.resolve(dto("paused"));
  await flush();
  assert.equal(h.gate.owes(), false);
  assert.equal(h.applied.length, 1);
});

test("a replaced client's override reply writes nothing and the override is re-sent through the new client", async () => {
  const h = harness();
  h.snapshot(dto("active"));
  h.gate.stop(false);
  h.swap({ name: "B" });
  h.posts[0]?.resolve(dto("paused")); // A answers after B became current
  await flush();
  assert.deepEqual(h.applied, [], "A's DTO is not applied to B's session");
  assert.equal(h.gate.owes(), true);
  h.snapshot(dto("active")); // B still shows the takeover active
  assert.equal(h.posts.at(-1)?.client, "B");
});

test("the viewer's Stop overrides even without a held takeover, once a client exists", () => {
  const h = harness();
  h.swap(null);
  h.gate.stop(true);
  h.swap({ name: "B" });
  assert.equal(h.posts.length, 1, "forced: no snapshot needed");
});

test("a hung override is aborted after its bound and retried by the next snapshot", async () => {
  const h = harness();
  h.snapshot(dto("active"));
  h.gate.stop(false);
  h.clock.advance(OVERRIDE_ATTEMPT_MS);
  await flush();
  assert.equal(h.posts[0]?.signal.aborted, true);
  h.snapshot(dto("active"));
  assert.equal(h.posts.length, 2);
});
