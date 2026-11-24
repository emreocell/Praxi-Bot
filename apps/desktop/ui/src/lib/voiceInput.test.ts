import assert from "node:assert/strict";
import { test } from "node:test";
import {
  IDLE,
  type DictationEvent,
  type DictationState,
  detectPlatform,
  isPttPress,
  levelVerdict,
  micDeniedStepsKey,
  parsePttKey,
  parseSilenceStopSec,
  stepDictation
} from "./voiceInput.ts";

function run(events: DictationEvent[], from: DictationState = IDLE) {
  let state = from;
  const effects: string[] = [];
  for (const ev of events) {
    const r = stepDictation(state, ev);
    state = r.state;
    if (r.effect !== "none") effects.push(r.effect);
  }
  return { state, effects };
}

test("hold to talk: press opens the mic, release stops and sends", () => {
  const r = run([
    { type: "press", via: "ptt" },
    { type: "micReady" },
    { type: "release" },
    { type: "stopped", hasAudio: true },
    { type: "sent" }
  ]);
  assert.deepEqual(r.effects, ["openMic", "stopRecording"]);
  assert.deepEqual(r.state, IDLE);
});

test("releasing before the microphone is open discards instead of sending", () => {
  const r = run([{ type: "press", via: "ptt" }, { type: "release" }, { type: "micReady" }]);
  assert.deepEqual(r.effects, ["openMic", "discard"]);
  assert.deepEqual(r.state, IDLE);
});

test("click mode: release does nothing, a second click or silence stops", () => {
  const held = run([{ type: "press", via: "click" }, { type: "micReady" }, { type: "release" }]);
  assert.equal(held.state.phase, "recording");
  assert.deepEqual(held.effects, ["openMic"]);
  assert.deepEqual(run([{ type: "click" }], held.state).effects, ["stopRecording"]);
  assert.deepEqual(run([{ type: "silence" }], held.state).effects, ["stopRecording"]);
});

test("an empty recording or a failed microphone returns to idle without sending", () => {
  assert.deepEqual(
    run([{ type: "press", via: "click" }, { type: "micReady" }, { type: "stopped", hasAudio: false }]).state,
    IDLE
  );
  assert.deepEqual(run([{ type: "press", via: "click" }, { type: "micFailed" }]).state, IDLE);
});

test("a second press while busy is ignored", () => {
  const r = run([{ type: "press", via: "ptt" }, { type: "press", via: "ptt" }]);
  assert.deepEqual(r.effects, ["openMic"]);
});

test("push-to-talk key: only function-style keys, auto-repeat and chords ignored", () => {
  assert.equal(parsePttKey("KeyA"), "F9");
  assert.equal(parsePttKey("F12"), "F12");
  assert.equal(isPttPress({ code: "F9", repeat: true }, "F9"), false);
  assert.equal(isPttPress({ code: "F9", repeat: false, ctrlKey: true }, "F9"), false);
  assert.equal(isPttPress({ code: "F9", repeat: false }, "F9"), true);
});

test("silence timeout accepts only the offered choices", () => {
  assert.equal(parseSilenceStopSec(3), 3);
  assert.equal(parseSilenceStopSec(4), 0);
  assert.equal(parseSilenceStopSec("3"), 0);
});

test("OS steps and microphone verdicts", () => {
  assert.equal(micDeniedStepsKey(detectPlatform("Mozilla/5.0 (Windows NT 10.0)")), "voice.input.denied.windows");
  assert.equal(micDeniedStepsKey(detectPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X)")), "voice.input.denied.mac");
  assert.equal(micDeniedStepsKey(detectPlatform("X11; Linux x86_64")), "voice.input.denied.linux");
  assert.equal(levelVerdict(0), "silent");
  assert.equal(levelVerdict(0.03), "quiet");
  assert.equal(levelVerdict(0.3), "good");
});
