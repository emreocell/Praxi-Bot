/** Gemini Live takes raw 16-bit little-endian PCM, 16 kHz, mono (docs/design/voice-chat.md). */
export const GEMINI_SAMPLE_RATE = 16_000;

/** Mix channels down to mono. */
export function mixToMono(channels: Float32Array[]): Float32Array {
  const first = channels[0];
  if (!first) {
    return new Float32Array(0);
  }
  if (channels.length === 1) {
    return first;
  }
  const out = new Float32Array(first.length);
  for (const ch of channels) {
    for (let i = 0; i < out.length; i += 1) {
      out[i] = (out[i] ?? 0) + (ch[i] ?? 0) / channels.length;
    }
  }
  return out;
}

/** Resample by averaging the source samples that fall into each target sample (fine for speech). */
export function resample(input: Float32Array, from: number, to: number): Float32Array {
  if (from === to || input.length === 0) {
    return input;
  }
  const ratio = from / to;
  const out = new Float32Array(Math.max(1, Math.floor(input.length / ratio)));
  for (let i = 0; i < out.length; i += 1) {
    const start = Math.floor(i * ratio);
    const end = Math.min(input.length, Math.max(start + 1, Math.floor((i + 1) * ratio)));
    let sum = 0;
    for (let j = start; j < end; j += 1) {
      sum += input[j] ?? 0;
    }
    out[i] = sum / (end - start);
  }
  return out;
}

/** Float samples in [-1, 1] to 16-bit little-endian PCM bytes. */
export function floatToPcm16(input: Float32Array): Uint8Array {
  const out = new Uint8Array(input.length * 2);
  const view = new DataView(out.buffer);
  for (let i = 0; i < input.length; i += 1) {
    const s = Math.max(-1, Math.min(1, input[i] ?? 0));
    view.setInt16(i * 2, Math.round(s < 0 ? s * 0x8000 : s * 0x7fff), true);
  }
  return out;
}

function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

/** Decode a recording (webm/opus, ...) with the WebView's own decoder into 16 kHz mono PCM16. */
export async function recordingToPcm16k(blob: Blob): Promise<Uint8Array> {
  const ctx = new AudioContext();
  try {
    const buf = await ctx.decodeAudioData(await blob.arrayBuffer());
    const channels = Array.from({ length: buf.numberOfChannels }, (_, i) => buf.getChannelData(i));
    return floatToPcm16(resample(mixToMono(channels), buf.sampleRate, GEMINI_SAMPLE_RATE));
  } finally {
    void ctx.close().catch(() => undefined);
  }
}

/** The `transcribe` request body for the active provider: the recording as is, or PCM for Gemini Live. */
export async function transcribePayload(
  blob: Blob,
  provider: "whisper" | "gemini_live" | undefined
): Promise<{ audio_base64: string; mime: string }> {
  if (provider === "gemini_live") {
    return { audio_base64: bytesToBase64(await recordingToPcm16k(blob)), mime: "audio/pcm" };
  }
  const type = blob.type || "audio/webm";
  return { audio_base64: bytesToBase64(new Uint8Array(await blob.arrayBuffer())), mime: type.split(";")[0] ?? type };
}
