import assert from "node:assert/strict";
import { test } from "node:test";
import { activeRunAfterCancel, composerButton, sendButtonDisabled, sendButtonMode } from "./composerRun.ts";

test("sendButtonMode switches to stop only while a run is active and the box is empty", () => {
  assert.equal(sendButtonMode({ runActive: false, hasText: false }), "send");
  assert.equal(sendButtonMode({ runActive: false, hasText: true }), "send");
  assert.equal(sendButtonMode({ runActive: true, hasText: false }), "stop");
  assert.equal(sendButtonMode({ runActive: true, hasText: true }), "send");
});

test("a finished event beats a stale running row, and Stopping returns to Send", () => {
  const finished = new Set(["run-1"]);
  assert.equal(
    activeRunAfterCancel({
      listedLiveId: "run-1",
      finishedByEvent: new Set(),
      eventLiveId: null,
      ackedId: null
    }),
    "run-1"
  );
  assert.equal(
    activeRunAfterCancel({
      listedLiveId: "run-1",
      finishedByEvent: finished,
      eventLiveId: "run-1",
      ackedId: "run-1"
    }),
    null
  );
  assert.equal(composerButton({ runActive: true, hasText: false, stopping: true }), "stopping");
  assert.equal(composerButton({ runActive: false, hasText: false, stopping: true }), "send");
  assert.equal(composerButton({ runActive: true, hasText: true, stopping: false }), "send");
  assert.equal(composerButton({ runActive: true, hasText: false, stopping: false }), "stop");
});

test("sendButtonDisabled checks stopping in stop mode and sending in send mode", () => {
  assert.equal(sendButtonDisabled({ mode: "send", sending: false, stopping: true }), false);
  assert.equal(sendButtonDisabled({ mode: "send", sending: true, stopping: false }), true);
  assert.equal(sendButtonDisabled({ mode: "stop", sending: true, stopping: false }), false);
  assert.equal(sendButtonDisabled({ mode: "stop", sending: false, stopping: true }), true);
});
