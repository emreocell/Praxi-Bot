import assert from "node:assert/strict";
import test from "node:test";
import type { ControlMode, ControlSnapshot } from "../overlay/events.ts";
import { startLocalComputerPresence } from "./localComputerPresence.ts";

test("host chat keeps the Windows daemon registered and reports human takeover locally", async () => {
  const seen: ControlMode[] = [];
  const idle: ControlSnapshot = {
    mode: "idle", runGeneration: 0, bot: null, currentStep: null, stopHotkey: "",
    activeTarget: null, lastClick: null, refusedActionsCount: 0, stateRev: 1
  };
  let report: (snapshot: ControlSnapshot) => void = () => {};
  const stop = startLocalComputerPresence({
    active: { putControlState: async () => assert.fail("the main bridge owns the host heartbeat") },
    local: { putControlState: async (mode) => { seen.push(mode); } },
    read: async () => idle,
    subscribe: (listener) => {
      report = listener;
      return () => {};
    },
    intervalMs: 60_000
  });
  try {
    assert.ok(stop);
    await new Promise(setImmediate);
    report({ ...idle, mode: "human_driving", stateRev: 2 });
    await new Promise(setImmediate);
    assert.deepEqual(seen, ["idle", "human_driving"]);
  } finally {
    stop?.();
  }
});
