/**
 * State machine of the hands-free voice call (docs/design/voice-call-mode.md). Pure: the hook feeds
 * it events from the microphone, the transcription call, the run and the speech queue; the overlay
 * renders its state. It knows nothing about approvals beyond "paused": a permission request is only
 * ever answered by a click on the approval card, never from here (no event of this machine can).
 */

export type CallPhase =
  | "idle" //      no call, microphone closed
  | "starting" //  asking for the microphone
  | "listening" // microphone open, waiting for / recording the owner
  | "thinking" //  transcribing, then the bot works on its answer
  | "speaking" //  the bot reads its answer aloud
  | "approval" //  the run waits for a click on the approval card; the loop is paused
  | "failed"; //   the call ended with an error the owner must read

/** i18n keys of the messages a call can show (all localized in en/tr/de). */
export type CallNoticeKey =
  | "voice.call.noSpeech"
  | "voice.call.transcribeFailed"
  | "voice.call.replyFailed"
  | "voice.call.notConfigured"
  | "voice.call.geminiKeyMissing"
  | "voice.call.wrongProvider"
  | "voice.call.runActive"
  | "voice.call.ownerRequired"
  | "voice.call.daemonUnavailable"
  | "voice.call.serviceRefused"
  | "voice.call.micTimeout"
  | "voice.micDenied"
  | "voice.micMissing"
  | "voice.micBusy"
  | "voice.call.unsupported";

export interface CallState {
  phase: CallPhase;
  /** Speech is being recorded right now (listening only). */
  heard: boolean;
  muted: boolean;
  /** Push-to-talk instead of automatic end-of-speech detection. */
  pushToTalk: boolean;
  captions: boolean;
  /** The reply of this turn is complete (nothing more will be spoken once the queue drains). */
  replyDone: boolean;
  /** A one-line message for the owner (transient while listening, final when failed). */
  notice: CallNoticeKey | null;
  /** Consecutive turns without speech. */
  silentTurns: number;
}

export type CallEvent =
  | { type: "START" }
  | { type: "FAILED"; notice: CallNoticeKey }
  | { type: "MIC_READY" }
  | { type: "MIC_FAILED"; notice: "voice.micDenied" | "voice.micMissing" | "voice.micBusy" | "voice.call.unsupported" }
  | { type: "SPEECH_START" }
  | { type: "SPEECH_END" }
  | { type: "NO_SPEECH" }
  | { type: "TRANSCRIBED"; empty: boolean }
  | { type: "TRANSCRIBE_FAILED"; notConfigured: boolean }
  | { type: "SPEAKING" }
  | { type: "SPEECH_DRAINED" }
  | { type: "REPLY_DONE" }
  | { type: "REPLY_FAILED" }
  | { type: "APPROVAL_PENDING" }
  | { type: "APPROVAL_CLEARED" }
  | { type: "INTERRUPT" }
  | { type: "TOGGLE_MUTE" }
  | { type: "TOGGLE_PUSH_TO_TALK" }
  | { type: "TOGGLE_CAPTIONS" }
  | { type: "END" };

export function initialCallState(captions = false): CallState {
  return { phase: "idle", heard: false, muted: false, pushToTalk: false, captions, replyDone: false, notice: null, silentTurns: 0 };
}

const listening = (s: CallState, patch: Partial<CallState> = {}): CallState => ({
  ...s,
  phase: "listening",
  heard: false,
  replyDone: false,
  notice: null,
  ...patch
});

/** Whether the microphone stream may be open in this phase (a visible call is the only reason). */
export function micOpen(phase: CallPhase): boolean {
  return phase !== "idle" && phase !== "failed";
}

export function callReducer(s: CallState, e: CallEvent): CallState {
  // Toggles and start/end work in every phase of a live call.
  switch (e.type) {
    case "TOGGLE_CAPTIONS":
      return { ...s, captions: !s.captions };
    case "TOGGLE_MUTE":
      return s.phase === "idle" || s.phase === "failed" ? s : { ...s, muted: !s.muted, heard: false };
    case "TOGGLE_PUSH_TO_TALK":
      return s.phase === "idle" || s.phase === "failed" ? s : { ...s, pushToTalk: !s.pushToTalk, heard: false };
    case "END":
      return { ...s, phase: "idle", heard: false, replyDone: false, notice: null, silentTurns: 0, muted: false };
    case "FAILED":
      return s.phase === "idle" ? s : { ...s, phase: "failed", heard: false, notice: e.notice };
    case "START":
      return s.phase === "idle" || s.phase === "failed"
        ? { ...initialCallState(s.captions), pushToTalk: s.pushToTalk, phase: "starting" }
        : s;
  }
  switch (s.phase) {
    case "starting":
      if (e.type === "MIC_READY") {
        return listening(s);
      }
      if (e.type === "MIC_FAILED") {
        return { ...s, phase: "failed", notice: e.notice };
      }
      return s;
    case "listening":
      switch (e.type) {
        case "SPEECH_START":
          return s.muted ? s : { ...s, heard: true, notice: null };
        case "SPEECH_END":
          return s.heard ? { ...s, phase: "thinking", heard: false, replyDone: false, notice: null } : s;
        case "NO_SPEECH":
          return { ...s, notice: "voice.call.noSpeech", silentTurns: s.silentTurns + 1 };
        case "APPROVAL_PENDING":
          return { ...s, phase: "approval", heard: false };
        default:
          return s;
      }
    case "thinking":
      switch (e.type) {
        case "TRANSCRIBED":
          return e.empty
            ? listening(s, { notice: "voice.call.noSpeech", silentTurns: s.silentTurns + 1 })
            : { ...s, silentTurns: 0 };
        case "TRANSCRIBE_FAILED":
          return e.notConfigured
            ? { ...s, phase: "failed", notice: "voice.call.notConfigured" }
            : listening(s, { notice: "voice.call.transcribeFailed" });
        case "SPEAKING":
          return { ...s, phase: "speaking" };
        case "REPLY_DONE":
          return listening(s);
        case "REPLY_FAILED":
          return listening(s, { notice: "voice.call.replyFailed" });
        case "APPROVAL_PENDING":
          return { ...s, phase: "approval" };
        default:
          return s;
      }
    case "speaking":
      switch (e.type) {
        case "REPLY_DONE":
          return { ...s, replyDone: true };
        case "SPEECH_DRAINED":
          return s.replyDone ? listening(s) : { ...s, phase: "thinking" };
        case "INTERRUPT":
          // Barge-in: the owner talks over the bot. The rest of the reply is dropped.
          return listening(s);
        case "REPLY_FAILED":
          return listening(s, { notice: "voice.call.replyFailed" });
        case "APPROVAL_PENDING":
          return { ...s, phase: "approval" };
        default:
          return s;
      }
    case "approval":
      switch (e.type) {
        case "APPROVAL_CLEARED":
          return { ...s, phase: "thinking" };
        case "REPLY_FAILED":
          return listening(s, { notice: "voice.call.replyFailed" });
        case "REPLY_DONE":
          return { ...s, replyDone: true };
        // Speech, microphone and transcript events are ignored while an approval waits: whatever
        // is said can never move the call past the card.
        default:
          return s;
      }
    default:
      return s;
  }
}
