import assert from "node:assert/strict";
import { test } from "node:test";
import { callReducer, initialCallState } from "./callMachine.ts";
import { callMicrophone, voiceConfigNotice, voiceFailureNotice } from "./readiness.ts";

const config = { stt_provider: "gemini_live", gemini_key_set: false, stt_key_set: false, stt_base_url: "https://api.openai.com/v1" };

test("startup explains missing keys and a mismatched provider without changing the provider", () => {
  assert.equal(voiceConfigNotice(config), "voice.call.geminiKeyMissing");
  assert.equal(voiceConfigNotice({ ...config, gemini_key_set: true }), null);
  assert.equal(voiceConfigNotice({ ...config, stt_provider: "whisper", gemini_key_set: true }), "voice.call.wrongProvider");
  assert.equal(voiceConfigNotice({ ...config, stt_provider: "whisper" }), "voice.call.notConfigured");
  assert.equal(voiceConfigNotice({ ...config, stt_provider: "whisper", stt_base_url: "http://localhost:8080/v1" }), null);
});

test("owner, busy, unavailable and refused routes give distinct recovery instructions", () => {
  assert.equal(voiceFailureNotice({ status: 403 }), "voice.call.ownerRequired");
  assert.equal(voiceFailureNotice({ status: 409, code: "voice_not_configured" }), "voice.call.notConfigured");
  assert.equal(voiceFailureNotice({ status: 409, code: "chat_busy" }), "voice.call.runActive");
  assert.equal(voiceFailureNotice({ status: 502 }), "voice.call.serviceRefused");
  assert.equal(voiceFailureNotice({ status: 503 }), "voice.call.daemonUnavailable");
});

test("startup failure is visible, retryable, and late permission events cannot reopen an ended call", () => {
  const starting = callReducer(initialCallState(), { type: "START" });
  const failed = callReducer(starting, { type: "FAILED", notice: "voice.call.runActive" });
  assert.equal(failed.phase, "failed");
  assert.equal(failed.notice, "voice.call.runActive");
  const retry = callReducer(failed, { type: "START" });
  assert.equal(retry.phase, "starting");
  assert.equal(retry.notice, null);
  const ended = callReducer(retry, { type: "END" });
  assert.equal(callReducer(ended, { type: "MIC_READY" }).phase, "idle");
  assert.equal(callReducer(ended, { type: "FAILED", notice: "voice.micDenied" }).phase, "idle");
});

test("a silent microphone prompt times out and a late granted stream is closed", async () => {
  let resolve!: (stream: MediaStream) => void;
  let stopped = false;
  const pending = callMicrophone(() => new Promise<MediaStream>((done) => { resolve = done; }), 5);
  await assert.rejects(pending, /voice.call.micTimeout/);
  resolve({ getTracks: () => [{ stop: () => { stopped = true; } }] } as unknown as MediaStream);
  await Promise.resolve();
  assert.equal(stopped, true);
});
