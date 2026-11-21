import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { callReducer, initialCallState, micOpen, type CallEvent, type CallState } from "./callMachine.ts";

const play = (events: CallEvent[], from: CallState = initialCallState()): CallState => events.reduce(callReducer, from);

test("a full turn: listening -> thinking -> speaking -> listening", () => {
  const s = play([{ type: "START" }, { type: "MIC_READY" }, { type: "SPEECH_START" }]);
  assert.equal(s.phase, "listening");
  assert.equal(s.heard, true);
  const t = play([{ type: "SPEECH_END" }, { type: "TRANSCRIBED", empty: false }, { type: "SPEAKING" }], s);
  assert.equal(t.phase, "speaking");
  const done = play([{ type: "REPLY_DONE" }, { type: "SPEECH_DRAINED" }], t);
  assert.equal(done.phase, "listening");
});

test("speech drained before the reply is complete goes back to thinking, not listening", () => {
  const s = play([{ type: "START" }, { type: "MIC_READY" }, { type: "SPEECH_START" }, { type: "SPEECH_END" }, { type: "SPEAKING" }]);
  assert.equal(callReducer(s, { type: "SPEECH_DRAINED" }).phase, "thinking");
});

test("barge-in while speaking returns to listening; interrupt elsewhere does nothing", () => {
  const speaking = play([{ type: "START" }, { type: "MIC_READY" }, { type: "SPEECH_START" }, { type: "SPEECH_END" }, { type: "SPEAKING" }]);
  assert.equal(callReducer(speaking, { type: "INTERRUPT" }).phase, "listening");
  const listening = play([{ type: "START" }, { type: "MIC_READY" }]);
  assert.equal(callReducer(listening, { type: "INTERRUPT" }), listening);
});

test("errors: mic denied fails the call with its message, empty speech and provider failure keep it going", () => {
  const denied = play([{ type: "START" }, { type: "MIC_FAILED", notice: "voice.micDenied" }]);
  assert.deepEqual([denied.phase, denied.notice], ["failed", "voice.micDenied"]);
  assert.equal(micOpen(denied.phase), false);
  const thinking = play([{ type: "START" }, { type: "MIC_READY" }, { type: "SPEECH_START" }, { type: "SPEECH_END" }]);
  const empty = callReducer(thinking, { type: "TRANSCRIBED", empty: true });
  assert.deepEqual([empty.phase, empty.notice], ["listening", "voice.call.noSpeech"]);
  const provider = callReducer(thinking, { type: "TRANSCRIBE_FAILED", notConfigured: false });
  assert.deepEqual([provider.phase, provider.notice], ["listening", "voice.call.transcribeFailed"]);
  assert.equal(callReducer(thinking, { type: "TRANSCRIBE_FAILED", notConfigured: true }).phase, "failed");
});

test("end closes the microphone from every phase", () => {
  for (const events of [[], [{ type: "MIC_READY" }], [{ type: "MIC_READY" }, { type: "SPEECH_START" }, { type: "SPEECH_END" }]] as CallEvent[][]) {
    const s = play([{ type: "START" }, ...events, { type: "END" }]);
    assert.equal(s.phase, "idle");
    assert.equal(micOpen(s.phase), false);
  }
});

test("an approval pauses the call and nothing said can move it past the card", () => {
  const waiting = play([{ type: "START" }, { type: "MIC_READY" }, { type: "SPEECH_START" }, { type: "SPEECH_END" }, { type: "APPROVAL_PENDING" }]);
  assert.equal(waiting.phase, "approval");
  const noise: CallEvent[] = [
    { type: "SPEECH_START" }, { type: "SPEECH_END" }, { type: "TRANSCRIBED", empty: false }, { type: "INTERRUPT" }, { type: "SPEAKING" }, { type: "SPEECH_DRAINED" }
  ];
  assert.equal(play(noise, waiting).phase, "approval");
  assert.equal(callReducer(waiting, { type: "APPROVAL_CLEARED" }).phase, "thinking");
});

test("voice call code has no way to answer an approval (click-only, ADR 0021)", () => {
  const dirs = [new URL(".", import.meta.url), new URL("../../components/voiceCall/", import.meta.url)];
  for (const dir of dirs) {
    for (const f of readdirSync(dir).filter((n) => /\.(ts|tsx)$/.test(n) && !n.endsWith(".test.ts"))) {
      const src = readFileSync(new URL(f, dir), "utf8");
      assert.doesNotMatch(src, /decideApproval|decideCliInstall|\/approvals|\.approve\(|\.deny\(/, f);
    }
  }
});
