/**
 * Gapless playback of the model's speech, with "flush on interrupt". The audio device is behind
 * `PlaybackSink`, so the scheduling is testable without Web Audio: chunks are queued back to back
 * on the sink's clock and `flush` stops everything that was scheduled but not yet played out.
 */
import { OUTPUT_RATE, pcm16ToFloat } from "./pcm.ts";

export interface ScheduledSound {
  stop(): void;
}

export interface PlaybackSink {
  /** Seconds on the device clock. */
  now(): number;
  /** Play `samples` (mono, `OUTPUT_RATE`) at `at` seconds on the device clock. */
  play(samples: Float32Array, at: number, onEnded: () => void): ScheduledSound;
}

/** Audio queued ahead of the clock before the first chunk plays: smooths network jitter. */
const LEAD_IN_SECONDS = 0.06;

export interface PlaybackQueue {
  /** Queue PCM16 speech (24 kHz mono). */
  push(pcm: Uint8Array): void;
  /** Stop and forget everything queued (the owner spoke over the model, or pressed stop). */
  flush(): void;
  /** Speech is queued or playing. */
  readonly busy: boolean;
}

export function createPlaybackQueue(sink: PlaybackSink, onIdle: () => void): PlaybackQueue {
  let playAt = 0;
  const live = new Set<ScheduledSound>();
  return {
    push(pcm) {
      const samples = pcm16ToFloat(pcm);
      if (samples.length === 0) {
        return;
      }
      const now = sink.now();
      if (playAt < now + (live.size === 0 ? LEAD_IN_SECONDS : 0)) {
        playAt = now + LEAD_IN_SECONDS;
      }
      const sound: ScheduledSound = sink.play(samples, playAt, () => {
        // A flushed sound is already forgotten: its late `ended` must not report idle twice.
        if (live.delete(sound) && live.size === 0) {
          onIdle();
        }
      });
      live.add(sound);
      playAt += samples.length / OUTPUT_RATE;
    },
    flush() {
      const stopped = [...live];
      live.clear();
      playAt = 0;
      for (const sound of stopped) {
        sound.stop();
      }
    },
    get busy() {
      return live.size > 0;
    }
  };
}
