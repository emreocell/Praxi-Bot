import assert from "node:assert/strict";
import { test } from "node:test";
import { base64ToBytes, bytesToBase64, CHUNK_BYTES, createChunker, frameToPcm16, levelOf, pcm16ToFloat } from "./pcm.ts";

test("a 48 kHz frame becomes 16 kHz mono PCM16 (one third of the samples, 2 bytes each)", () => {
  const frame = new Float32Array(4800).fill(0.5);
  const pcm = frameToPcm16([frame], 48_000);
  assert.equal(pcm.length, 1600 * 2);
  assert.equal(new DataView(pcm.buffer).getInt16(0, true), Math.round(0.5 * 0x7fff));
});

test("two channels are mixed down to one", () => {
  const pcm = frameToPcm16([new Float32Array(160).fill(1), new Float32Array(160).fill(-1)], 16_000);
  assert.equal(pcm.length, 320);
  assert.equal(new DataView(pcm.buffer).getInt16(0, true), 0);
});

test("loud samples clip instead of wrapping", () => {
  const pcm = frameToPcm16([new Float32Array([2, -2])], 16_000);
  const view = new DataView(pcm.buffer);
  assert.equal(view.getInt16(0, true), 0x7fff);
  assert.equal(view.getInt16(2, true), -0x8000);
});

test("PCM16 from the model decodes to floats and ignores a trailing odd byte", () => {
  const bytes = new Uint8Array([0x00, 0x40, 0x00, 0xc0, 0x7f]);
  const out = pcm16ToFloat(bytes);
  assert.deepEqual([...out], [0.5, -0.5]);
});

test("PCM16 decoding honours the byte offset of a view", () => {
  const backing = new Uint8Array([9, 9, 0x00, 0x40]);
  assert.deepEqual([...pcm16ToFloat(backing.subarray(2))], [0.5]);
});

test("base64 round-trips binary audio, also larger than one conversion block", () => {
  const bytes = Uint8Array.from({ length: 100_000 }, (_, i) => i % 256);
  assert.deepEqual(base64ToBytes(bytesToBase64(bytes)), bytes);
});

test("the chunker hands out exact 100 ms chunks and keeps the remainder", () => {
  const chunker = createChunker();
  assert.equal(CHUNK_BYTES, 3200);
  assert.equal(chunker.push(new Uint8Array(1000)).length, 0);
  const out = chunker.push(new Uint8Array(6000));
  assert.deepEqual(out.map((c) => c.length), [3200, 3200]);
  // 7000 in, 6400 out: 600 left, so 2600 more completes the next chunk.
  assert.equal(chunker.push(new Uint8Array(2600)).length, 1);
  chunker.reset();
  assert.equal(chunker.push(new Uint8Array(3199)).length, 0);
});

test("the chunker keeps the order of the bytes", () => {
  const chunker = createChunker(4);
  const a = chunker.push(new Uint8Array([1, 2, 3]));
  const b = chunker.push(new Uint8Array([4, 5, 6, 7, 8, 9]));
  assert.deepEqual([...a, ...b].map((c) => [...c]), [[1, 2, 3, 4], [5, 6, 7, 8]]);
});

test("level is 0 for silence and grows with loudness, capped at 1", () => {
  assert.equal(levelOf(new Float32Array(0)), 0);
  assert.equal(levelOf(new Float32Array(100)), 0);
  const quiet = levelOf(new Float32Array(100).fill(0.05));
  const loud = levelOf(new Float32Array(100).fill(0.2));
  assert.ok(quiet > 0 && loud > quiet);
  assert.equal(levelOf(new Float32Array(100).fill(1)), 1);
});
