/** Whether a live conversation can start: it needs a Gemini key, whatever the dictation provider is. */
import type { LiveNoticeKey } from "./machine.ts";

export interface LiveVoiceReadinessConfig {
  gemini_key_set: boolean;
  use_provider_key?: boolean;
  gemini_provider_key_available?: boolean;
}

/** Only metadata is read: the key itself never reaches the UI. */
export function liveVoiceNotice(config: LiveVoiceReadinessConfig): LiveNoticeKey | null {
  const reuses = config.use_provider_key !== false && config.gemini_provider_key_available === true;
  return config.gemini_key_set || reuses ? null : "voice.live.notice.keyMissing";
}
