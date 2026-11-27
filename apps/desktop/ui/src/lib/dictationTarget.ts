/**
 * Where a dictation recording is transcribed. The microphone is on THIS computer, so the owner's own
 * daemon goes first, whichever computer the bot works on. The active computer (a paired server) is
 * tried only after this computer's daemon answered 409 `voice_not_configured`. If this computer's
 * daemon is not there at all, dictation is reported as not set up: the recording is never sent to a
 * second machine because the first one was missing or failed.
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
  return err?.status === 409 && err.code === "voice_not_configured";
}

/** Dictation has nowhere to go on this computer; looks like the daemon's own 409 to the caller. */
export class DictationNotConfigured extends Error {
  readonly status = 409;
  readonly code = "voice_not_configured";
  constructor() {
    super("dictation is not set up on this computer");
  }
}

async function transcribeWith(
  ep: SttEndpoint,
  blob: Blob,
  language: string,
  encode: (blob: Blob, provider: SttProvider) => Promise<{ audio_base64: string; mime: string }>
): Promise<string> {
  const { stt_provider } = await ep.getVoice();
  const out = await ep.transcribe({
    ...(await encode(blob, stt_provider)),
    language,
    local_only: stt_provider === "local_voice"
  });
  return out.text;
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
  const { local, active, language, encode } = opts;
  if (!local) {
    throw new DictationNotConfigured();
  }
  try {
    return await transcribeWith(local, blob, language, encode);
  } catch (e) {
    // Only "no service set up here" moves on. A failing or unreachable service is reported as it is.
    if (!isNotConfigured(e) || !active || active.baseUrl === local.baseUrl) {
      throw e;
    }
  }
  return transcribeWith(active, blob, language, encode);
}
