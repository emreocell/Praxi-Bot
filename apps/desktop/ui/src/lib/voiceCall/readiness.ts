import type { CallNoticeKey } from "./callMachine.ts";

export type VoiceReadinessConfig = { stt_provider: string; gemini_key_set: boolean; stt_key_set: boolean; stt_base_url: string };

/** Inspect only metadata. A stored Gemini key never silently changes the owner's chosen provider. */
export function voiceConfigNotice(config: VoiceReadinessConfig): CallNoticeKey | null {
  if (config.stt_provider === "gemini_live") {
    return config.gemini_key_set ? null : "voice.call.geminiKeyMissing";
  }
  if (config.stt_provider !== "whisper") return "voice.call.wrongProvider";
  // Local Whisper servers can intentionally run without an API key.
  let local = false;
  try {
    local = ["127.0.0.1", "localhost", "[::1]"].includes(new URL(config.stt_base_url).hostname);
  } catch { /* An invalid endpoint needs setup too. */ }
  if (config.stt_key_set || local) return null;
  return config.gemini_key_set ? "voice.call.wrongProvider" : "voice.call.notConfigured";
}

/** Notices fixed in Settings → Voice → Dictation service: the card offers "Open settings" for them. */
const SETTINGS_NOTICES: ReadonlySet<CallNoticeKey> = new Set<CallNoticeKey>([
  "voice.call.geminiKeyMissing",
  "voice.call.notConfigured",
  "voice.call.wrongProvider",
  "voice.call.serviceRefused"
]);
export function noticeNeedsSettings(notice: CallNoticeKey | null): boolean {
  return notice !== null && SETTINGS_NOTICES.has(notice);
}

/** Longest a startup step (voice settings, owner key, audio resume) may take before the call says so. */
export const STEP_DEADLINE_MS = 8_000;

/** Rejects with `voice.call.stepTimeout` when `work` does not settle in time; never leaves the UI waiting. */
export async function withDeadline<T>(work: Promise<T>, ms = STEP_DEADLINE_MS): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("voice.call.stepTimeout")), ms);
      })
    ]);
  } finally { clearTimeout(timer); }
}

const lastVoiceConfig = new Map<string, VoiceReadinessConfig>();
/** The voice settings last seen for this daemon (loaded with the chat, or saved in Settings). */
export function rememberVoiceConfig(baseUrl: string | undefined, config: VoiceReadinessConfig): void {
  if (baseUrl) lastVoiceConfig.set(baseUrl, config);
}
export function knownVoiceConfig(baseUrl: string | undefined): VoiceReadinessConfig | undefined {
  return baseUrl ? lastVoiceConfig.get(baseUrl) : undefined;
}

/** Never display raw daemon/upstream messages, which can contain provider details. */
export function voiceFailureNotice(error: unknown): CallNoticeKey {
  const e = error as { status?: number; code?: string } | null;
  if (e?.status === 401 || e?.status === 403) return "voice.call.ownerRequired";
  if ((error as Error | null)?.message === "voice.call.stepTimeout") return "voice.call.stepTimeout";
  if (e?.code === "voice_not_configured") return "voice.call.notConfigured";
  if (e?.code === "chat_busy" || e?.code === "bot_busy" || e?.status === 409) return "voice.call.runActive";
  if (e?.status === 502 || e?.status === 422) return "voice.call.serviceRefused";
  return "voice.call.daemonUnavailable";
}

/** A dismissed WebView permission prompt must not leave Connecting on screen forever. */
export async function callMicrophone(request: () => Promise<MediaStream>, timeoutMs = 20_000): Promise<MediaStream> {
  let expired = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      request().then((stream) => {
        if (expired) stream.getTracks().forEach((track) => track.stop());
        return stream;
      }),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          expired = true;
          reject(new Error("voice.call.micTimeout"));
        }, timeoutMs);
      })
    ]);
  } finally { clearTimeout(timer); }
}
