import assert from "node:assert/strict";
import { test } from "node:test";
import { createVad, LISTEN_VAD, rmsOf, type VadEvent } from "./vad.ts";

/** Feed `level` for `ms` in 50 ms frames; returns the events with their time. */
function run(vad: ReturnType<typeof createVad>, from: number, ms: number, level: number): { at: number; ev: VadEvent }[] {
  const out: { at: number; ev: VadEvent }[] = [];
  for (let t = from; t < from + ms; t += 50) {
    const ev = vad.feed(level, t);
    if (ev) {
      out.push({ at: t, ev });
    }
  }
  return out;
}

test("speech starts after the onset time and ends after the silence timeout", () => {
  const vad = createVad(LISTEN_VAD);
  vad.reset(0);
  assert.deepEqual(run(vad, 0, 500, 0.002), []);
  const start = run(vad, 500, 1000, 0.2);
  assert.deepEqual(start.map((e) => e.ev), ["speech_start"]);
  const end = run(vad, 1500, 2000, 0.002);
  assert.deepEqual(end.map((e) => e.ev), ["speech_end"]);
  assert.ok((end[0]?.at ?? 0) >= 1500 + LISTEN_VAD.silenceMs);
});

test("a short click is ignored and a noisy room does not count as speech", () => {
  const vad = createVad(LISTEN_VAD);
  vad.reset(0);
  assert.deepEqual(run(vad, 0, 100, 0.3), []); // 100 ms < onset
  const noisy = createVad(LISTEN_VAD);
  noisy.reset(0);
  assert.deepEqual(run(noisy, 0, 3000, 0.015), []); // below the absolute floor
});

test("no speech for the whole window reports no_speech; a very long utterance is cut", () => {
  const vad = createVad({ ...LISTEN_VAD, noSpeechMs: 2000 });
  vad.reset(0);
  assert.deepEqual(run(vad, 0, 2500, 0.001).map((e) => e.ev), ["no_speech"]);
  const long = createVad({ ...LISTEN_VAD, maxSpeechMs: 3000 });
  long.reset(0);
  assert.ok(run(long, 0, 4000, 0.3).some((e) => e.ev === "too_long"));
});

test("rms of a constant signal is its amplitude", () => {
  assert.equal(rmsOf([]), 0);
  assert.ok(Math.abs(rmsOf([0.5, -0.5, 0.5, -0.5]) - 0.5) < 1e-9);
});
