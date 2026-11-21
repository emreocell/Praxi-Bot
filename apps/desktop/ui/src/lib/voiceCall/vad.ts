/**
 * Energy-based voice activity detection for the hands-free call (docs/design/voice-call-mode.md).
 * Pure: it is fed one loudness value (RMS, 0..1) per audio frame plus a clock, so it is tested
 * without a microphone. No audio is kept here, only a few numbers.
 */

export interface VadConfig {
  /** Absolute floor: quieter than this is never speech, however quiet the room is. */
  minThreshold: number;
  /** Speech must be this many times louder than the tracked room noise. */
  noiseFactor: number;
  /** Loud for this long (ms) before speech counts as started (ignores clicks and taps). */
  onsetMs: number;
  /** Quiet for this long (ms) after speech ends the utterance. */
  silenceMs: number;
  /** Utterances shorter than this (ms) are treated as noise and dropped. */
  minSpeechMs: number;
  /** Nothing heard for this long (ms) after listening started: "no speech detected". */
  noSpeechMs: number;
  /** Hard cap of one utterance (ms), same as dictation. */
  maxSpeechMs: number;
}

export const LISTEN_VAD: VadConfig = {
  minThreshold: 0.02,
  noiseFactor: 3,
  onsetMs: 150,
  silenceMs: 1200,
  minSpeechMs: 350,
  noSpeechMs: 10_000,
  maxSpeechMs: 120_000
};

/** While the bot speaks: stricter, so its own voice leaking into the microphone does not cut it off. */
export const BARGE_IN_VAD: VadConfig = {
  ...LISTEN_VAD,
  minThreshold: 0.06,
  noiseFactor: 5,
  onsetMs: 400
};

export type VadEvent = "speech_start" | "speech_end" | "no_speech" | "too_long";

export interface Vad {
  /** Feed one frame; returns the event it caused, if any. */
  feed(rms: number, nowMs: number): VadEvent | null;
  /** Start over (new listening turn). */
  reset(nowMs: number): void;
  /** The current speech threshold (for tests and a level meter). */
  threshold(): number;
}

/** Root-mean-square loudness of one block of float samples (-1..1). */
export function rmsOf(samples: ArrayLike<number>): number {
  if (samples.length === 0) {
    return 0;
  }
  let sum = 0;
  for (let i = 0; i < samples.length; i++) {
    const v = samples[i] ?? 0;
    sum += v * v;
  }
  return Math.sqrt(sum / samples.length);
}

export function createVad(config: VadConfig): Vad {
  let floor = 0;
  let started = 0;
  let loudSince: number | null = null;
  let speechStart: number | null = null;
  let quietSince: number | null = null;
  let primed = false;

  const threshold = () => Math.max(config.minThreshold, floor * config.noiseFactor);

  return {
    threshold,
    reset(nowMs) {
      started = nowMs;
      loudSince = null;
      speechStart = null;
      quietSince = null;
      primed = false;
    },
    feed(rms, nowMs) {
      if (!primed) {
        // The first frame seeds the noise floor so a noisy room does not read as speech.
        floor = Math.min(rms, config.minThreshold);
        started = started || nowMs;
        primed = true;
      }
      const loud = rms > threshold();
      if (speechStart === null) {
        if (loud) {
          loudSince ??= nowMs;
          if (nowMs - loudSince >= config.onsetMs) {
            speechStart = loudSince;
            quietSince = null;
            return "speech_start";
          }
          return null;
        }
        loudSince = null;
        // Track the room noise only while nobody speaks: fast down, slow up.
        floor += (rms - floor) * (rms < floor ? 0.3 : 0.05);
        if (nowMs - started >= config.noSpeechMs) {
          started = nowMs;
          return "no_speech";
        }
        return null;
      }
      if (nowMs - speechStart >= config.maxSpeechMs) {
        speechStart = null;
        return "too_long";
      }
      if (loud) {
        quietSince = null;
        return null;
      }
      quietSince ??= nowMs;
      if (nowMs - quietSince >= config.silenceMs) {
        const spoke = quietSince - speechStart;
        speechStart = null;
        quietSince = null;
        loudSince = null;
        // A short blip (door, cough) is dropped: keep listening.
        return spoke >= config.minSpeechMs ? "speech_end" : null;
      }
      return null;
    }
  };
}
