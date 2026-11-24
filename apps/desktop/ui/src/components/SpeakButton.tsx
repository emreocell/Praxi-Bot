import { Square, Volume2 } from "lucide-react";
import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { effectiveVoice } from "../lib/botVoice";
import { markdownToSpeech } from "../lib/speechText";
import { currentSpeakingId, speak, speechSupported, stopSpeaking, subscribeSpeaking } from "../lib/speaker";
import { DEFAULT_SETTINGS, loadSettingsSync } from "../store/settingsStore";
import styles from "./MessageInfo.module.css";

function voiceOptions(botId: string | null | undefined, prefs: { voiceName: string; voiceRate: number }) {
  const v = effectiveVoice(botId, { voiceName: prefs.voiceName, rate: prefs.voiceRate });
  return { voiceName: v.voiceName, rate: v.rate };
}

/** Starts reading `text` aloud with the bot's voice (settings are read now, so a changed voice applies at once). */
export function speakReply(
  id: string,
  text: string,
  botId: string | null | undefined,
  t: (key: string) => string,
  lang: string
): void {
  const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
  speak(id, markdownToSpeech(text, { code: t("voice.codeOmitted"), table: t("voice.tableOmitted") }), {
    ...voiceOptions(botId, prefs),
    lang
  });
}

/** "Read aloud" beside a bot reply (ADR 0021): OS speech synthesis, replies are never sent anywhere. */
export function SpeakButton({ id, text, botId }: { id: string; text: string; botId?: string | null }) {
  const { t, i18n } = useTranslation();
  const speakingId = useSyncExternalStore(subscribeSpeaking, currentSpeakingId, currentSpeakingId);
  if (!speechSupported() || !text.trim()) {
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
      title={label}
      data-testid="speak-reply"
      onClick={() => {
        if (active) {
          stopSpeaking();
          return;
        }
        speakReply(id, text, botId, (k) => t(k), i18n.language);
      }}
    >
      {active ? <Square aria-hidden="true" /> : <Volume2 aria-hidden="true" />}
    </button>
  );
}
