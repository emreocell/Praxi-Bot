import assert from "node:assert/strict";
import { test } from "node:test";
import {
  initialLiveState,
  isOpen,
  liveFailureNotice,
  liveNoticeNeedsSettings,
  liveReducer,
  transcriptTurns,
  type LiveEvent,
  type LiveState
} from "./machine.ts";
import { liveVoiceNotice } from "./readiness.ts";

const run = (events: LiveEvent[], from: LiveState = initialLiveState()) => events.reduce(liveReducer, from);
const open = () => run([{ type: "START" }, { type: "READY", maxSeconds: 900 }]);

test("start asks for the microphone, ready opens the conversation", () => {
  assert.equal(run([{ type: "START" }]).phase, "starting");
  const s = open();
  assert.equal(s.phase, "listening");
  assert.equal(s.maxSeconds, 900);
  assert.equal(isOpen(s.phase), true);
});

test("fragments of one speaker merge into one line, a new speaker starts a new one", () => {
  const s = run([{ type: "YOU", text: "Hello " }, { type: "YOU", text: "there" }, { type: "BOT", text: "Hi!" }], open());
  assert.deepEqual(s.lines.map((l) => [l.role, l.text, l.final]), [["user", "Hello there", true], ["assistant", "Hi!", false]]);
});

test("turn_complete finalises every open line, and the next fragment starts a new line", () => {
  const s = run([{ type: "BOT", text: "Done." }, { type: "TURN_COMPLETE" }, { type: "BOT", text: "More." }], open());
  assert.deepEqual(s.lines.map((l) => [l.text, l.final]), [["Done.", true], ["More.", false]]);
});

test("audio makes the voice model 'speaking' until the playback drains", () => {
  let s = run([{ type: "AUDIO" }], open());
  assert.equal(s.phase, "speaking");
  assert.equal(s.botSpeaking, true);
  s = run([{ type: "PLAYBACK_IDLE" }], s);
  assert.equal(s.phase, "listening");
});

test("speaking over the model (interrupted) or pressing stop ends its speech and its line", () => {
  for (const type of ["INTERRUPTED", "STOP_TALKING"] as const) {
    const s = run([{ type: "AUDIO" }, { type: "BOT", text: "Let me explain" }, { type }], open());
    assert.equal(s.phase, "listening", type);
    assert.equal(s.lines[0]?.final, true, type);
  }
});

test("a handover shows 'working', then 'approval' while a click is awaited, then back", () => {
  let s = run([{ type: "TOOL_START" }], open());
  assert.equal(s.phase, "working");
  s = run([{ type: "APPROVAL", waiting: true }], s);
  assert.equal(s.phase, "approval");
  s = run([{ type: "APPROVAL", waiting: false }], s);
  assert.equal(s.phase, "working");
  s = run([{ type: "TOOL_DONE" }], s);
  assert.equal(s.phase, "listening");
});

test("working outranks speaking so the headline says the bot is busy", () => {
  const s = run([{ type: "TOOL_START" }, { type: "AUDIO" }], open());
  assert.equal(s.phase, "working");
  assert.equal(s.botSpeaking, true);
});

test("two handovers: 'working' lasts until both are done", () => {
  let s = run([{ type: "TOOL_START" }, { type: "TOOL_START" }, { type: "TOOL_DONE" }], open());
  assert.equal(s.phase, "working");
  s = run([{ type: "TOOL_DONE" }, { type: "TOOL_DONE" }], s);
  assert.equal(s.pending, 0);
  assert.equal(s.phase, "listening");
});

test("approval waiting is cleared when the last handover is answered", () => {
  const s = run([{ type: "TOOL_START" }, { type: "APPROVAL", waiting: true }, { type: "TOOL_DONE" }], open());
  assert.equal(s.approvalWaiting, false);
  assert.equal(s.phase, "listening");
});

test("reconnecting is a calm flag that clears when anything arrives, the conversation stays open", () => {
  let s = run([{ type: "RECONNECTING" }], open());
  assert.equal(s.reconnecting, true);
  assert.equal(s.phase, "listening");
  s = run([{ type: "YOU", text: "hi" }], s);
  assert.equal(s.reconnecting, false);
});

test("mute toggles while open and is kept", () => {
  let s = run([{ type: "TOGGLE_MUTE" }], open());
  assert.equal(s.muted, true);
  s = run([{ type: "YOU", text: "x" }], s);
  assert.equal(s.muted, true);
  assert.equal(run([{ type: "TOGGLE_MUTE" }]).muted, false, "nothing to mute while idle");
});

test("ended keeps the words, closes the open line and carries the reason's notice", () => {
  const s = run([{ type: "BOT", text: "Bye" }, { type: "ENDED", reason: "limit" }], open());
  assert.equal(s.phase, "ended");
  assert.equal(s.notice, "voice.live.notice.limit");
  assert.equal(s.lines[0]?.final, true);
  assert.equal(run([{ type: "ENDED", reason: "closed" }], open()).notice, null);
});

test("events after the end are ignored", () => {
  const ended = run([{ type: "ENDED", reason: "closed" }], open());
  assert.equal(liveReducer(ended, { type: "YOU", text: "late" }), ended);
  assert.equal(liveReducer(ended, { type: "AUDIO" }), ended);
});

test("a failed start carries the notice and the settings action for a missing key", () => {
  const s = run([{ type: "START" }, { type: "FAILED", notice: "voice.live.notice.keyMissing" }]);
  assert.equal(s.phase, "failed");
  assert.equal(liveNoticeNeedsSettings(s.notice), true);
  assert.equal(liveNoticeNeedsSettings("voice.live.notice.busy"), false);
  assert.equal(run([{ type: "RESET" }], s).phase, "idle");
});

test("the transcript is trimmed, ordered and drops empty lines", () => {
  const s = run(
    [{ type: "YOU", text: "  What is\n the weather? " }, { type: "TURN_COMPLETE" }, { type: "BOT", text: "   " }, { type: "BOT", text: "Sunny." }, { type: "TURN_COMPLETE" }],
    open()
  );
  assert.deepEqual(transcriptTurns(s.lines), [
    { role: "user", text: "What is the weather?" },
    { role: "assistant", text: "Sunny." }
  ]);
});

test("failures map to a notice, never to raw daemon text", () => {
  assert.equal(liveFailureNotice({ status: 409, code: "live_voice_not_configured" }), "voice.live.notice.keyMissing");
  assert.equal(liveFailureNotice({ status: 409, code: "conflict" }), "voice.live.notice.busy");
  assert.equal(liveFailureNotice({ status: 502, code: "voice_upstream_failed", message: "key=SECRET" }), "voice.live.notice.serviceFailed");
  assert.equal(liveFailureNotice({ status: 403, code: "owner_key_required" }), "voice.live.notice.ownerRequired");
  assert.equal(liveFailureNotice({ status: 422 }), "voice.live.notice.settingsInvalid");
  assert.equal(liveFailureNotice({ status: 503 }), "voice.live.notice.unavailable");
  assert.equal(liveFailureNotice({ message: "voice.call.stepTimeout" }), "voice.call.stepTimeout");
  assert.equal(liveFailureNotice(new Error("fetch failed")), "voice.live.notice.daemonUnavailable");
});

test("live voice needs a Gemini key, whatever the dictation provider is", () => {
  assert.equal(liveVoiceNotice({ gemini_key_set: false }), "voice.live.notice.keyMissing");
  assert.equal(liveVoiceNotice({ gemini_key_set: true }), null);
  assert.equal(liveVoiceNotice({ gemini_key_set: false, gemini_provider_key_available: true }), null);
  assert.equal(liveVoiceNotice({ gemini_key_set: false, gemini_provider_key_available: true, use_provider_key: false }), "voice.live.notice.keyMissing");
});
