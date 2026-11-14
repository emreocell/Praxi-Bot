import assert from "node:assert/strict";
import { test } from "node:test";
import type { Run } from "../api/types.ts";
import { cancelAllRuns } from "./runState.ts";

function run(state: string): Run {
  return {
    id: state, state, bot_id: "bot", chat_id: null, kind: "turn", error: null,
    created_at_ms: 1, updated_at_ms: 1, can_remember: false
  };
}

test("tray Stop All cancels queued and paused daemon runs as well as executing runs", async () => {
  const cancelled: string[] = [];
  await cancelAllRuns({
    listRuns: async () => ["queued", "running", "waiting_approval", "paused", "succeeded", "failed", "cancelled"].map(run),
    cancelRun: async (id) => {
      cancelled.push(id);
      if (id === "running") throw new Error("already finished during cancellation");
      return run("cancelled");
    }
  });
  assert.deepEqual(cancelled, ["queued", "running", "waiting_approval", "paused"]);
});

test("an unavailable run snapshot cannot cancel runs on an assumed target", async () => {
  const cancelled: string[] = [];
  await assert.rejects(cancelAllRuns({
    listRuns: async () => { throw new Error("offline"); },
    cancelRun: async (id) => { cancelled.push(id); return run("cancelled"); }
  }), /offline/);
  assert.deepEqual(cancelled, []);
});
