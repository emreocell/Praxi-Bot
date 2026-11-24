/**
 * Voice input preferences and the dictation state machine (push-to-talk, silence auto-stop,
 * microphone choice). Pure: no microphone, no DOM, so it is tested without hardware.
 */

export const DEFAULT_PTT_KEY = "F9";
export const SILENCE_STOP_CHOICES = [0, 2, 3, 5, 8] as const;
export const DEFAULT_SILENCE_STOP_SEC = 0;

/** Keys allowed for push-to-talk: F1-F24, Pause, ScrollLock. They never type text, so holding one is safe. */
export function isValidPttKey(code: unknown): code is string {
  return typeof code === "string" && /^(F([1-9]|1[0-9]|2[0-4])|Pause|ScrollLock)$/.test(code);
}

export function parsePttKey(v: unknown): string {
  return isValidPttKey(v) ? v : DEFAULT_PTT_KEY;
}

export function parseSilenceStopSec(v: unknown): number {
  return typeof v === "number" && (SILENCE_STOP_CHOICES as readonly number[]).includes(v) ? v : DEFAULT_SILENCE_STOP_SEC;
}

export function parseMicDeviceId(v: unknown): string {
  return typeof v === "string" ? v.slice(0, 300) : "";
}

/** The "F9" style label shown to people. */
export function pttKeyLabel(code: string): string {
  return code === "ScrollLock" ? "Scroll Lock" : code;
}

/** Whether a keyboard event is the push-to-talk press (the held key repeating does not count). */
export function isPttPress(
  e: { code: string; repeat: boolean; ctrlKey?: boolean; altKey?: boolean; metaKey?: boolean },
  key: string
): boolean {
  return e.code === key && !e.repeat && !e.ctrlKey && !e.altKey && !e.metaKey;
}

// -- dictation state machine -------------------------------------------------------------

export type DictationState =
  | { phase: "idle" }
  | { phase: "starting"; via: "click" | "ptt"; releasedEarly: boolean }
  | { phase: "recording"; via: "click" | "ptt" }
  | { phase: "sending" };

export type DictationEvent =
  | { type: "press"; via: "click" | "ptt" }
  | { type: "release" }
  | { type: "click" }
  | { type: "micReady" }
  | { type: "micFailed" }
  | { type: "silence" }
  | { type: "stopped"; hasAudio: boolean }
  | { type: "sent" };

export type DictationEffect = "openMic" | "stopRecording" | "discard" | "none";

export const IDLE: DictationState = { phase: "idle" };

/**
 * One step: the next state and what the caller must do. Rules: a click toggles; a push-to-talk
 * press starts and the release (not a second press) stops; a release before the microphone is
 * open cancels the recording instead of sending an empty clip; silence stops like a click.
 */
export function stepDictation(
  state: DictationState,
  ev: DictationEvent
): { state: DictationState; effect: DictationEffect } {
  switch (state.phase) {
    case "idle":
      if (ev.type === "press") {
        return { state: { phase: "starting", via: ev.via, releasedEarly: false }, effect: "openMic" };
      }
      break;
    case "starting":
      if (ev.type === "release" && state.via === "ptt") {
        return { state: { ...state, releasedEarly: true }, effect: "none" };
      }
      if (ev.type === "micReady") {
        return state.releasedEarly
          ? { state: IDLE, effect: "discard" }
          : { state: { phase: "recording", via: state.via }, effect: "none" };
      }
      if (ev.type === "micFailed") {
        return { state: IDLE, effect: "none" };
      }
      break;
    case "recording":
      if ((ev.type === "release" && state.via === "ptt") || ev.type === "click" || ev.type === "silence") {
        return { state, effect: "stopRecording" };
      }
      if (ev.type === "stopped") {
        return ev.hasAudio ? { state: { phase: "sending" }, effect: "none" } : { state: IDLE, effect: "none" };
      }
      if (ev.type === "micFailed") {
        return { state: IDLE, effect: "discard" };
      }
      break;
    case "sending":
      if (ev.type === "sent") {
        return { state: IDLE, effect: "none" };
      }
      break;
  }
  return { state, effect: "none" };
}

// -- permission / platform help ----------------------------------------------------------

export type Platform = "windows" | "mac" | "linux";

export function detectPlatform(userAgent: string): Platform {
  if (/Windows/i.test(userAgent)) return "windows";
  if (/Mac OS X|Macintosh/i.test(userAgent)) return "mac";
  return "linux";
}

/** i18n key of the exact steps that re-enable the microphone on this operating system. */
export function micDeniedStepsKey(p: Platform): `voice.input.denied.${Platform}` {
  return `voice.input.denied.${p}`;
}

/** A plain verdict for the peak level reached during a microphone test. */
export function levelVerdict(peak: number): "silent" | "quiet" | "good" {
  if (peak < 0.01) return "silent";
  if (peak < 0.06) return "quiet";
  return "good";
}

/** Level meter value 0..1 from an RMS (speech sits around 0.05-0.3, so scale up). */
export function meterFraction(rms: number): number {
  return Math.max(0, Math.min(1, rms * 4));
}
