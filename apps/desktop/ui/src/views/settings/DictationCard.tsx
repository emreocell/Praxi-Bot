import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { VoiceConfig, VoiceConfigInput } from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { SettingRow } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import { sttKeyInput, type SttProvider } from "../../lib/voiceAudio";
import { useDaemon } from "../../state/DaemonContext";
import { LocalVoicePanel } from "./LocalVoicePanel";

/** Where dictation / voice-call audio is transcribed; keys are write-only (secret store). */
export function DictationCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [config, setConfig] = useState<VoiceConfig | null>(null);
  const [provider, setProvider] = useState<VoiceConfig["stt_provider"]>("whisper");
  const [url, setUrl] = useState("");
  const [model, setModel] = useState("");
  const [geminiModel, setGeminiModel] = useState("");
  const [localModel, setLocalModel] = useState("whisper-small");
  const [drafts, setDrafts] = useState<Partial<Record<SttProvider, string>>>({});
  const [busy, setBusy] = useState(false);

  const apply = useCallback((c: VoiceConfig) => {
    setConfig(c);
    setProvider(c.stt_provider);
    setUrl(c.stt_base_url);
    setModel(c.stt_model);
    setGeminiModel(c.gemini_model);
    setLocalModel(c.local_model);
  }, []);
  useEffect(() => {
    client?.getVoice().then(apply).catch(() => undefined);
  }, [client, apply]);

  const gemini = provider === "gemini_live";
  const local = provider === "local_voice";
  const keyStored = gemini ? config?.gemini_key_set : config?.stt_key_set;

  const save = async (extra: Partial<VoiceConfigInput> = {}) => {
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      apply(
        await client.updateVoice({
          stt_provider: provider,
          stt_base_url: url,
          stt_model: model,
          gemini_model: geminiModel,
          local_model: localModel,
          ...extra
        })
      );
      setDrafts({});
      toast.show(t("voice.stt.saved"), "success");
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  if (!config) {
    return null;
  }
  const privacy = local ? t("voice.stt.localPrivacy") : gemini ? t("voice.stt.geminiPrivacy") : t("voice.stt.privacy");
  const key = provider === "local_voice" ? "" : (drafts[provider] ?? "");
  return (
    <SettingRow stacked title={t("voice.stt.title")} description={`${t("voice.stt.description")} ${privacy}`}>
      <select
        className={formStyles.input}
        aria-label={t("voice.stt.provider")}
        value={provider}
        onChange={(e) => setProvider(e.target.value as VoiceConfig["stt_provider"])}
      >
        <option value="whisper">{t("voice.stt.providerWhisper")}</option>
        <option value="gemini_live">{t("voice.stt.providerGemini")}</option>
        <option value="local_voice">{t("voice.stt.providerLocal")}</option>
      </select>
      {local ? (
        <LocalVoicePanel model={localModel} onModel={setLocalModel} />
      ) : gemini ? (
        <input
          className={formStyles.input}
          aria-label={t("voice.stt.geminiModel")}
          value={geminiModel}
          placeholder="gemini-3.5-transcribe-live"
          onChange={(e) => setGeminiModel(e.target.value)}
        />
      ) : (
        <>
          <input
            className={formStyles.input}
            aria-label={t("voice.stt.url")}
            value={url}
            placeholder="https://api.openai.com/v1"
            onChange={(e) => setUrl(e.target.value)}
          />
          <input
            className={formStyles.input}
            aria-label={t("voice.stt.model")}
            value={model}
            placeholder="whisper-1"
            onChange={(e) => setModel(e.target.value)}
          />
        </>
      )}
      {local ? null : (
      <input
        className={formStyles.input}
        type="password"
        autoComplete="off"
        aria-label={gemini ? t("voice.stt.geminiKey") : t("voice.stt.key")}
        value={key}
        placeholder={keyStored ? t("voice.stt.keyStored") : gemini ? t("voice.stt.geminiKey") : t("voice.stt.key")}
        onChange={(e) => setDrafts((d) => ({ ...d, [provider]: e.target.value }))}
      />
      )}
      <Button size="sm" disabled={busy} onClick={() => void save(provider === "local_voice" ? {} : sttKeyInput(provider, drafts))}>
        {t("voice.stt.save")}
      </Button>
      {keyStored && !local ? (
        <Button
          size="sm"
          variant="ghost"
          disabled={busy}
          onClick={() => void save(gemini ? { clear_gemini_key: true } : { clear_stt_key: true })}
        >
          {t("voice.stt.clearKey")}
        </Button>
      ) : null}
    </SettingRow>
  );
}
