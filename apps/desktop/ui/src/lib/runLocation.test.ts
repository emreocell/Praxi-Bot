import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { readRunPlace, stopLiveRuns, stopResult, switchNeedsConfirm, writeRunPlace } from "./runLocation.ts";

beforeEach(() => {
  const mem = new Map<string, string>();
  (globalThis as unknown as { window: unknown }).window = {
    localStorage: { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) }
  };
});

test("the chosen place is remembered and junk is ignored", () => {
  assert.equal(readRunPlace(), null);
  writeRunPlace("this");
  assert.equal(readRunPlace(), "this");
  writeRunPlace("server");
  assert.equal(readRunPlace(), "server");
  window.localStorage.setItem("praxi.runLocation", "later");
  assert.equal(readRunPlace(), null);
});

test("switching asks only while something runs", () => {
  assert.equal(switchNeedsConfirm(0), false);
  assert.equal(switchNeedsConfirm(2), true);
});

test("the stop result is honest about zero and about a real count", () => {
  assert.deepEqual(stopResult(0), { key: "runLocation.stopNone", tone: "info" });
  assert.deepEqual(stopResult(1), { key: "runLocation.stopped", tone: "success" });
});

function fakeClient(live: string[], outcome: "cancelled" | "failed" = "cancelled") {
  const stops: unknown[] = [];
  return {
    stops,
    listRuns: async () => [
      ...live.map((id) => ({ id, state: "running" })),
      { id: "done", state: "succeeded" }
    ],
    stopRuns: async (body: { run_ids?: string[] }) => {
      stops.push(body);
      return {
        stop_id: "s",
        first_seen: true,
        results: (body.run_ids ?? []).map((run_id) => ({ run_id, outcome: outcome }))
      } as never;
    }
  };
}

test("the local stop cancels the live runs it lists and reports the true count", async () => {
  const c = fakeClient(["a", "b"]);
  assert.equal(await stopLiveRuns(c), 2);
  assert.deepEqual((c.stops[0] as { run_ids: string[]; scope: string }).run_ids, ["a", "b"]);
  assert.equal((c.stops[0] as { scope: string }).scope, "listed");
});

test("the local stop with nothing live sends nothing and says zero", async () => {
  const c = fakeClient([]);
  assert.equal(await stopLiveRuns(c), 0);
  assert.equal(c.stops.length, 0);
});

test("a stop the daemon could not carry out throws instead of claiming success", async () => {
  await assert.rejects(stopLiveRuns(fakeClient(["a"], "failed")));
});
