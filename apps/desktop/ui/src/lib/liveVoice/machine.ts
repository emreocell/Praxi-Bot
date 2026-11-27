/**
 * State machine of the live voice conversation (docs/design/voice-live.md). Pure: the hook feeds it
 * what the event stream, the microphone and the bot's run report; the panel renders its state.
 *
 * It knows nothing about permissions. A tool call of the voice model is only ever handed over to the
 * bot as an ordinary chat message; whether that needs an approval is decided by the permission
 * engine and answered by a click on the card in the chat. No event of this machine can approve.
 */
import type { VoiceTurn } from "../../api/types.ts";

export type LivePhase =
  | "idle" //      nothing open
  | "starting" //  asking for the microphone, opening the session
  | "listening" // microphone open, waiting for the owner
  | "speaking" //  the voice model is talking
  | "working" //   the bot is doing something the owner asked for
  | "approval" //  the bot waits for a click on the approval card
  | "ended" //     over; the transcript is saved
  | "failed"; //   could not start or broke down; the owner must read why

/** i18n keys of the one-line messages a conversation can show. */
export type LiveNoticeKey =
  | "voice.live.notice.keyMissing"
  | "voice.live.notice.busy"
  | "voice.live.notice.ownerRequired"
  | "voice.live.notice.serviceFailed"
  | "voice.live.notice.unavailable"
  | "voice.live.notice.settingsInvalid"
  | "voice.live.notice.daemonUnavailable"
  | "voice.live.notice.dropped"
  | "voice.live.notice.limit"
  | "voice.live.notice.idle"
  | "voice.live.notice.error"
  | "voice.call.micTimeout"
  | "voice.call.stepTimeout"
  | "voice.live.notice.runActive"
  | "voice.micDenied"
  | "voice.micMissing"
  | "voice.micBusy"
  | "voice.call.unsupported";

export interface LiveLine {
  id: number;
  role: "user" | "assistant";
  text: string;
  /** Nothing more will be added (the turn is complete, or the other side spoke). */
  final: boolean;
}

export interface LiveState {
  phase: LivePhase;
  muted: boolean;
  lines: LiveLine[];
  notice: LiveNoticeKey | null;
  /** The connection to the voice service is being renewed; speech continues in a moment. */
  reconnecting: boolean;
  /** Seconds the session may last (from the `ready` event). */
  maxSeconds: number;
  /** The voice model is talking (audio is arriving or playing). */
  botSpeaking: boolean;
  /** Handovers to the bot still running. */
  pending: number;
  /** The bot's run waits for a click on the approval card. */
  approvalWaiting: boolean;
  nextId: number;
}

export type LiveEvent =
  | { type: "START" }
  | { type: "READY"; maxSeconds: number }
  | { type: "FAILED"; notice: LiveNoticeKey }
  | { type: "YOU"; text: string }
  | { type: "BOT"; text: string }
  | { type: "AUDIO" }
  | { type: "PLAYBACK_IDLE" }
  | { type: "TURN_COMPLETE" }
  | { type: "INTERRUPTED" }
  | { type: "STOP_TALKING" }
  | { type: "TOOL_START" }
  | { type: "TOOL_DONE" }
  | { type: "APPROVAL"; waiting: boolean }
  | { type: "RECONNECTING" }
  | { type: "ENDED"; reason: "limit" | "idle" | "error" | "closed" }
  | { type: "TOGGLE_MUTE" }
  | { type: "RESET" };

export function initialLiveState(): LiveState {
  return {
    phase: "idle",
    muted: false,
    lines: [],
    notice: null,
    reconnecting: false,
    maxSeconds: 0,
    botSpeaking: false,
    pending: 0,
    approvalWaiting: false,
    nextId: 1
  };
}

/** Phases that mean the conversation is open (the microphone may be on). */
export function isOpen(phase: LivePhase): boolean {
  return phase === "listening" || phase === "speaking" || phase === "working" || phase === "approval";
}

/** What the panel's headline says while the conversation is open. */
function headline(s: LiveState): LivePhase {
  if (s.pending > 0) {
    return s.approvalWaiting ? "approval" : "working";
  }
  return s.botSpeaking ? "speaking" : "listening";
}

function settle(s: LiveState): LiveState {
  return isOpen(s.phase) ? { ...s, phase: headline(s) } : s;
}

/** A piece of speech text joins the open line of the same speaker, else it starts a new line. */
function addText(s: LiveState, role: LiveLine["role"], text: string): LiveState {
  if (text === "") {
    return s;
  }
  const last = s.lines[s.lines.length - 1];
  if (last && last.role === role && !last.final) {
    const lines = [...s.lines.slice(0, -1), { ...last, text: last.text + text }];
    return { ...s, lines };
  }
  // The other side starts talking: what was open before is complete.
  const closed = s.lines.map((l) => (l.final ? l : { ...l, final: true }));
  return { ...s, lines: [...closed, { id: s.nextId, role, text, final: false }], nextId: s.nextId + 1 };
}

function finalizeAll(s: LiveState): LiveState {
  return s.lines.some((l) => !l.final) ? { ...s, lines: s.lines.map((l) => (l.final ? l : { ...l, final: true })) } : s;
}

const ENDED_NOTICE: Record<"limit" | "idle" | "error" | "closed", LiveNoticeKey | null> = {
  limit: "voice.live.notice.limit",
  idle: "voice.live.notice.idle",
  error: "voice.live.notice.error",
  closed: null
};

export function liveReducer(s: LiveState, e: LiveEvent): LiveState {
  switch (e.type) {
    case "RESET":
      return initialLiveState();
    case "START":
      return { ...initialLiveState(), phase: "starting" };
    case "FAILED":
      return { ...finalizeAll(s), phase: "failed", notice: e.notice, botSpeaking: false, pending: 0, approvalWaiting: false, reconnecting: false };
    case "READY":
      return s.phase === "starting" ? { ...s, phase: "listening", maxSeconds: e.maxSeconds } : s;
    case "TOGGLE_MUTE":
      return isOpen(s.phase) || s.phase === "starting" ? { ...s, muted: !s.muted } : s;
    default:
      break;
  }
  // Everything below only means something while the conversation is open.
  if (!isOpen(s.phase)) {
    return s;
  }
  switch (e.type) {
    case "YOU":
      return settle({ ...addText(s, "user", e.text), reconnecting: false });
    case "BOT":
      return settle({ ...addText(s, "assistant", e.text), reconnecting: false });
    case "AUDIO":
      return s.botSpeaking && !s.reconnecting ? s : settle({ ...s, botSpeaking: true, reconnecting: false });
    case "PLAYBACK_IDLE":
      return s.botSpeaking ? settle({ ...s, botSpeaking: false }) : s;
    case "TURN_COMPLETE":
      return finalizeAll(s);
    case "INTERRUPTED":
    case "STOP_TALKING":
      return settle({ ...finalizeAll(s), botSpeaking: false });
    case "TOOL_START":
      return settle({ ...s, pending: s.pending + 1 });
    case "TOOL_DONE":
      return settle({ ...s, pending: Math.max(0, s.pending - 1), approvalWaiting: s.pending <= 1 ? false : s.approvalWaiting });
    case "APPROVAL":
      return settle({ ...s, approvalWaiting: e.waiting });
    case "RECONNECTING":
      return { ...s, reconnecting: true };
    case "ENDED":
      return {
        ...finalizeAll(s),
        phase: "ended",
        notice: ENDED_NOTICE[e.reason],
        botSpeaking: false,
        pending: 0,
        approvalWaiting: false,
        reconnecting: false
      };
    default:
      return s;
  }
}

/** The lines to write into the chat: finished wording only, in order, trimmed, no empty ones. */
export function transcriptTurns(lines: readonly LiveLine[]): VoiceTurn[] {
  return lines.flatMap((l) => {
    const text = l.text.replace(/\s+/g, " ").trim();
    return text ? [{ role: l.role, text }] : [];
  });
}

/** Maps a failed open/stream call to the notice the owner reads (never the raw daemon text). */
export function liveFailureNotice(error: unknown): LiveNoticeKey {
  const e = error as { status?: number; code?: string; message?: string } | null;
  if (e?.message === "voice.call.stepTimeout") return "voice.call.stepTimeout";
  if (e?.code === "live_voice_not_configured" || e?.code === "voice_not_configured") return "voice.live.notice.keyMissing";
  if (e?.code === "owner_key_required" || e?.status === 401 || e?.status === 403) return "voice.live.notice.ownerRequired";
  if (e?.code === "conflict" || e?.status === 409) return "voice.live.notice.busy";
  if (e?.status === 422) return "voice.live.notice.settingsInvalid";
  if (e?.status === 502 || e?.code === "voice_upstream_failed") return "voice.live.notice.serviceFailed";
  if (e?.status === 503 || e?.status === 404) return "voice.live.notice.unavailable";
  return "voice.live.notice.daemonUnavailable";
}

/** Notices fixed in Settings → General → Voice dictation: the panel offers "Open settings". */
export function liveNoticeNeedsSettings(notice: LiveNoticeKey | null): boolean {
  return notice === "voice.live.notice.keyMissing" || notice === "voice.live.notice.settingsInvalid";
}
