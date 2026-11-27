import { Square, Volume2 } from "lucide-react";
import { useCallback, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { effectiveVoice } from "../lib/botVoice";
import { markdownToSpeech } from "../lib/speechText";
import { ApiError } from "../api/client";
import { currentSpeakingId, speak, speakOffline, speechSupported, stopSpeaking, subscribeSpeaking } from "../lib/speaker";
import { useLocalDaemonClient } from "../state/DaemonContext";
import { DEFAULT_SETTINGS, loadSettingsSync } from "../store/settingsStore";
import tipStyles from "./ActionTip.module.css";
import styles from "./MessageInfo.module.css";
import { useToast } from "./Toast";

function voiceOptions(botId: string | null | undefined, prefs: { voiceName: string; voiceRate: number }) {
  const v = effectiveVoice(botId, { voiceName: prefs.voiceName, rate: prefs.voiceRate });
  return { voiceName: v.voiceName, rate: v.rate };
}

export function replySpeechSupported(): boolean {
  return (loadSettingsSync() ?? DEFAULT_SETTINGS).offlineReadAloud || speechSupported();
}

export function useReplySpeaker() {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const client = useLocalDaemonClient();
  const lang = i18n.language;
  return useCallback((id: string, text: string, botId?: string | null) => {
    const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
    const spoken = markdownToSpeech(text, { code: t("voice.codeOmitted"), table: t("voice.tableOmitted") });
    const opts = { ...voiceOptions(botId, prefs), lang };
    if (!prefs.offlineReadAloud) {
      speak(id, spoken, opts);
      return;
    }
    if (!client) {
      toast.show(t("voice.offlineReadAloud.unavailable"), "danger");
      return;
    }
    void speakOffline(id, spoken, { ...opts, synthesize: (piece, language, signal) => client.synthesizeVoice(piece, language, signal) })
      .catch((e: unknown) => {
        const key = e instanceof ApiError && e.status === 409
          ? "notInstalled"
          : e instanceof ApiError && e.status === 429 ? "busy" : "playbackFailed";
        toast.show(t(OFFLINE_ERRORS[key]), "danger");
      });
  }, [client, lang, t, toast]);
}

const OFFLINE_ERRORS = {
  notInstalled: "voice.offlineReadAloud.notInstalled",
  busy: "voice.offlineReadAloud.busy",
  playbackFailed: "voice.offlineReadAloud.playbackFailed"
} as const;

/** "Read aloud" beside a bot reply (ADR 0021). */
export function SpeakButton({ id, text, botId }: { id: string; text: string; botId?: string | null }) {
  const { t } = useTranslation();
  const speakReply = useReplySpeaker();
  const speakingId = useSyncExternalStore(subscribeSpeaking, currentSpeakingId, currentSpeakingId);
  if (!replySpeechSupported() || !text.trim()) {
    return null;
  }
  const active = speakingId === id;
  const label = t(active ? "voice.stop" : "voice.speak");
  return (
    <button
      type="button"
      className={styles.info}
      aria-pressed={active}
      aria-label={label}
      data-testid="speak-reply"
      onClick={() => {
        if (active) {
          stopSpeaking();
          return;
        }
        speakReply(id, text, botId);
      }}
    >
      {active ? <Square aria-hidden="true" /> : <Volume2 aria-hidden="true" />}
      <span className={tipStyles.tip} aria-hidden="true">{label}</span>
    </button>
  );
}
