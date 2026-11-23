import assert from "node:assert/strict";
import { test } from "node:test";
import { floatToPcm16, mixToMono, resample, sttKeyInput, transcribePayload } from "./voiceAudio.ts";

test("floats become clamped little-endian 16-bit PCM", () => {
  const pcm = floatToPcm16(Float32Array.from([0, 1, -1, 2]));
  const view = new DataView(pcm.buffer);
  assert.deepEqual([0, 1, 2, 3].map((i) => view.getInt16(i * 2, true)), [0, 32767, -32768, 32767]);
});

test("48 kHz is averaged down to 16 kHz and stereo mixed to mono", () => {
  assert.deepEqual(Array.from(resample(Float32Array.from([1, 1, 1, 0, 0, 0]), 48_000, 16_000)), [1, 0]);
  assert.deepEqual(Array.from(mixToMono([Float32Array.from([1, 0]), Float32Array.from([0, 1])])), [0.5, 0.5]);
});

test("each speech provider keeps its own key draft", () => {
  // A Whisper key typed before switching the select must never be saved as the Gemini key.
  assert.deepEqual(sttKeyInput("gemini_live", { whisper: "sk-whisper" }), {});
  assert.deepEqual(sttKeyInput("whisper", { whisper: "sk-whisper", gemini_live: "g" }), { stt_key: "sk-whisper" });
  assert.deepEqual(sttKeyInput("gemini_live", { whisper: "sk-whisper", gemini_live: " g " }), { gemini_key: "g" });
});

test("the mock call works with the Gemini provider (no audio decoding)", async () => {
  const out = await transcribePayload(new Blob(["mock"], { type: "audio/webm" }), "gemini_live", { mock: true });
  assert.equal(out.mime, "audio/webm");
  await assert.rejects(transcribePayload(new Blob(["mock"]), "gemini_live"));
});

test("on-device voice also gets 16 kHz PCM, never the raw recording", async () => {
  const out = await transcribePayload(new Blob(["mock"], { type: "audio/webm" }), "local_voice", { mock: true });
  assert.equal(out.mime, "audio/webm");
  await assert.rejects(transcribePayload(new Blob(["mock"]), "local_voice"));
});
