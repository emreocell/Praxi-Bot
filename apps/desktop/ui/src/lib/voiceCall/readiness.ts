import type { CallNoticeKey } from "./callMachine.ts";

type Config = { stt_provider: string; gemini_key_set: boolean; stt_key_set: boolean; stt_base_url: string };

/** Inspect only metadata. A stored Gemini key never silently changes the owner's chosen provider. */
export function voiceConfigNotice(config: Config): CallNoticeKey | null {
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

/** Never display raw daemon/upstream messages, which can contain provider details. */
export function voiceFailureNotice(error: unknown): CallNoticeKey {
  const e = error as { status?: number; code?: string } | null;
  if (e?.status === 401 || e?.status === 403) return "voice.call.ownerRequired";
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
