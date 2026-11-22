import assert from "node:assert/strict";
import { test } from "node:test";
import { floatToPcm16, mixToMono, resample } from "./voiceAudio.ts";

test("floats become clamped little-endian 16-bit PCM", () => {
  const pcm = floatToPcm16(Float32Array.from([0, 1, -1, 2]));
  const view = new DataView(pcm.buffer);
  assert.deepEqual([0, 1, 2, 3].map((i) => view.getInt16(i * 2, true)), [0, 32767, -32768, 32767]);
});

test("48 kHz is averaged down to 16 kHz and stereo mixed to mono", () => {
  assert.deepEqual(Array.from(resample(Float32Array.from([1, 1, 1, 0, 0, 0]), 48_000, 16_000)), [1, 0]);
  assert.deepEqual(Array.from(mixToMono([Float32Array.from([1, 0]), Float32Array.from([0, 1])])), [0.5, 0.5]);
});
