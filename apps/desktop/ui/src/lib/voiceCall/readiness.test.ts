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

test("a reused provider key counts as configured unless the owner turned reuse off", () => {
  const reuse = { ...config, use_provider_key: true, gemini_provider_key_available: true };
  assert.equal(voiceConfigNotice(reuse), null);
  assert.equal(voiceConfigNotice({ ...reuse, use_provider_key: false }), "voice.call.geminiKeyMissing");
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

import { readFileSync } from "node:fs";
import { knownVoiceConfig, noticeNeedsSettings, rememberVoiceConfig, withDeadline } from "./readiness.ts";

test("remembered settings give the answer at once, with no waiting step", () => {
  assert.equal(knownVoiceConfig("http://127.0.0.1:1"), undefined);
  rememberVoiceConfig("http://127.0.0.1:1", config);
  const known = knownVoiceConfig("http://127.0.0.1:1");
  assert.ok(known);
  // Synchronous: a plain return value, not a promise.
  assert.equal(voiceConfigNotice(known), "voice.call.geminiKeyMissing");
});

test("a startup step that never answers ends with a visible timeout notice", async () => {
  const never = new Promise<never>(() => undefined);
  await assert.rejects(withDeadline(never, 5), /voice.call.stepTimeout/);
  assert.equal(voiceFailureNotice(new Error("voice.call.stepTimeout")), "voice.call.stepTimeout");
  assert.equal(await withDeadline(Promise.resolve(7), 50), 7);
});

test("settings notices offer Open settings; others do not", () => {
  for (const n of ["voice.call.geminiKeyMissing", "voice.call.notConfigured", "voice.call.wrongProvider", "voice.call.serviceRefused"] as const) {
    assert.equal(noticeNeedsSettings(n), true);
  }
  assert.equal(noticeNeedsSettings("voice.call.runActive"), false);
  assert.equal(noticeNeedsSettings(null), false);
});

test("messages name the real settings place (Voice page, Dictation service card) in every language", () => {
  for (const l of ["en", "tr", "de"]) {
    const d = JSON.parse(readFileSync(new URL(`../../i18n/locales/${l}.json`, import.meta.url), "utf8"));
    const page: string = d.settingsModal.pages.voice.name;
    const card: string = d.voice.stt.title;
    for (const k of ["geminiKeyMissing", "notConfigured", "serviceRefused", "wrongProvider"]) {
      assert.ok(d.voice.call[k].includes(`${page} → ${card}`), `${l}.voice.call.${k}`);
    }
    assert.ok(d.voice.notConfigured.includes(`${page} → ${card}`), `${l}.voice.notConfigured`);
  }
});

test("the call card layers above the side panels and the failed state is a real dialog", () => {
  const css = readFileSync(new URL("../../components/voiceCall/VoiceCallPanel.module.css", import.meta.url), "utf8");
  const modalCss = readFileSync(new URL("../../components/Modal.module.css", import.meta.url), "utf8");
  const z = (c: string) => Number(/\.(?:panel|backdrop) \{[^}]*?z-index: (\d+)/.exec(c)?.[1]);
  assert.ok(z(css) > 40, "docked call panel must sit above the side panels (40)");
  assert.ok(z(css) < z(modalCss), "dialogs stay above the docked call");
  const tsx = readFileSync(new URL("../../components/voiceCall/VoiceCallPanel.tsx", import.meta.url), "utf8");
  assert.match(tsx, /createPortal\(/, "docked panel is portaled out of the chat's stacking context");
  assert.match(tsx, /<Modal open onClose=\{call\.end\}/, "failed state uses the shared Modal (trap, Escape, focus return)");
});
