/**
 * Where a dictation recording is transcribed. The microphone is on THIS computer, so the owner's own
 * daemon goes first, whichever computer the bot works on. The active computer (a paired server) is
 * only a fallback when this computer has no transcription service set up. Any other failure is
 * reported as it is: the recording is never silently sent to a second machine.
 */

export type SttProvider = "whisper" | "gemini_live" | "local_voice";

export interface TranscribeRequest {
  audio_base64: string;
  mime: string;
  language?: string;
  local_only?: boolean;
}

/** The two calls dictation needs from a daemon client. */
export interface SttEndpoint {
  baseUrl: string;
  getVoice(): Promise<{ stt_provider: SttProvider }>;
  transcribe(input: TranscribeRequest): Promise<{ text: string }>;
}

/** The daemon answered 409 `voice_not_configured`: no transcription service is set up there. */
export function isNotConfigured(e: unknown): boolean {
  const err = e as { status?: unknown; code?: unknown } | null;
  return err?.status === 409 && (err.code === undefined || err.code === "voice_not_configured");
}

/** Local daemon first, then the active computer when it is a different daemon. */
export function dictationEndpoints<T extends { baseUrl: string }>(local: T | null, active: T | null): T[] {
  const list = [local, active].filter((c): c is T => c !== null);
  return list.filter((c, i) => list.findIndex((o) => o.baseUrl === c.baseUrl) === i);
}

export async function transcribeRecording(
  blob: Blob,
  opts: {
    local: SttEndpoint | null;
    active: SttEndpoint | null;
    language: string;
    encode: (blob: Blob, provider: SttProvider) => Promise<{ audio_base64: string; mime: string }>;
  }
): Promise<string> {
  const endpoints = dictationEndpoints(opts.local, opts.active);
  let notConfigured: unknown = null;
  for (const ep of endpoints) {
    try {
      const { stt_provider } = await ep.getVoice();
      const out = await ep.transcribe({
        ...(await opts.encode(blob, stt_provider)),
        language: opts.language,
        local_only: stt_provider === "local_voice"
      });
      return out.text;
    } catch (e) {
      if (!isNotConfigured(e)) {
        throw e;
      }
      notConfigured = e;
    }
  }
  throw notConfigured ?? new Error("no daemon to transcribe with");
}
