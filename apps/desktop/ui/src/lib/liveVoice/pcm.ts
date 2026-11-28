/**
 * Audio helpers of the live voice conversation (docs/design/voice-live.md). Pure, no Web Audio.
 * The microphone goes out as 16 kHz mono 16-bit PCM in ~100 ms chunks; the model's speech comes
 * back as 24 kHz mono 16-bit PCM.
 */
import { floatToPcm16, GEMINI_SAMPLE_RATE, mixToMono, resample } from "../voiceAudio.ts";

export const INPUT_RATE = GEMINI_SAMPLE_RATE;
export const OUTPUT_RATE = 24_000;
/** 100 ms of 16 kHz 16-bit mono audio. */
export const CHUNK_BYTES = (INPUT_RATE / 10) * 2;

/** One microphone frame (any channel count and rate) as 16 kHz mono PCM16 bytes. */
export function frameToPcm16(channels: Float32Array[], fromRate: number): Uint8Array {
  return floatToPcm16(resample(mixToMono(channels), fromRate, INPUT_RATE));
}

/** Little-endian PCM16 bytes to float samples in [-1, 1). An odd trailing byte is ignored. */
export function pcm16ToFloat(bytes: Uint8Array): Float32Array {
  const n = Math.floor(bytes.length / 2);
  const out = new Float32Array(n);
  const view = new DataView(bytes.buffer, bytes.byteOffset, n * 2);
  for (let i = 0; i < n; i += 1) {
    out[i] = view.getInt16(i * 2, true) / 0x8000;
  }
  return out;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

export function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) {
    out[i] = bin.charCodeAt(i);
  }
  return out;
}

/** Collects PCM16 bytes and hands them out in chunks of `size` bytes (the last partial one stays). */
export function createChunker(size: number = CHUNK_BYTES): { push(bytes: Uint8Array): Uint8Array[]; reset(): void } {
  let pending = new Uint8Array(0);
  return {
    push(bytes) {
      const merged = new Uint8Array(pending.length + bytes.length);
      merged.set(pending);
      merged.set(bytes, pending.length);
      const out: Uint8Array[] = [];
      let at = 0;
      while (merged.length - at >= size) {
        out.push(merged.slice(at, at + size));
        at += size;
      }
      pending = merged.slice(at);
      return out;
    },
    reset() {
      pending = new Uint8Array(0);
    }
  };
}

/** Loudness of float samples, 0..1 (RMS). */
export function levelOf(samples: ArrayLike<number>): number {
  if (samples.length === 0) {
    return 0;
  }
  let sum = 0;
  for (let i = 0; i < samples.length; i += 1) {
    const v = samples[i] ?? 0;
    sum += v * v;
  }
  return Math.min(1, Math.sqrt(sum / samples.length) * 4);
}
