import assert from "node:assert/strict";
import { test } from "node:test";
import { sendButtonDisabled, sendButtonMode } from "./composerRun.ts";

test("sendButtonMode switches to stop only while a run is active and the box is empty", () => {
  assert.equal(sendButtonMode({ runActive: false, hasText: false }), "send");
  assert.equal(sendButtonMode({ runActive: false, hasText: true }), "send");
  assert.equal(sendButtonMode({ runActive: true, hasText: false }), "stop");
  assert.equal(sendButtonMode({ runActive: true, hasText: true }), "send");
});

test("sendButtonDisabled checks stopping in stop mode and sending in send mode", () => {
  assert.equal(sendButtonDisabled({ mode: "send", sending: false, stopping: true }), false);
  assert.equal(sendButtonDisabled({ mode: "send", sending: true, stopping: false }), true);
  assert.equal(sendButtonDisabled({ mode: "stop", sending: true, stopping: false }), false);
  assert.equal(sendButtonDisabled({ mode: "stop", sending: false, stopping: true }), true);
});
