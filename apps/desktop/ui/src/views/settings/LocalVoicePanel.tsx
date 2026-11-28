import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../../api/client";
import type { LocalVoiceStatus } from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { Switch } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import { useDaemon, useLocalDaemonClient } from "../../state/DaemonContext";

type Props =
  | { kind: "stt"; model: string; onModel: (id: string) => void }
  | { kind: "tts"; enabled: boolean; onEnabled: (enabled: boolean) => void };

const WHISPER_TEXT = {
  "whisper-tiny": { label: "voice.offlineModels.tinyLabel", description: "voice.offlineModels.tinyDescription" },
  "whisper-base": { label: "voice.offlineModels.baseLabel", description: "voice.offlineModels.baseDescription" },
  "whisper-small": { label: "voice.offlineModels.smallLabel", description: "voice.offlineModels.smallDescription" }
} as const;

function whisperText(id: string) {
  return Object.hasOwn(WHISPER_TEXT, id) ? WHISPER_TEXT[id as keyof typeof WHISPER_TEXT] : null;
}

/** Picker, licence and download button for the offline (on-device) speech models. */
export function LocalVoicePanel(props: Props) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client: active } = useDaemon();
  const localApi = useLocalDaemonClient();
  const client = props.kind === "tts" ? localApi : active;
  const [snapshot, setSnapshot] = useState<{ client: ApiClient; status: LocalVoiceStatus } | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const status = snapshot?.client === client ? snapshot.status : null;

  const refresh = useCallback(async () => {
    try {
      if (!client) return null;
      const next = await client.getLocalVoice();
      setSnapshot({ client, status: next });
      return next;
    } catch {
      return null;
    }
  }, [client]);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const current = props.kind === "tts" ? status?.tts_model : status?.models.find((m) => m.id === props.model) ?? status?.models[0];
  const installing = status?.installing ?? null;
  const thisModel = installing !== null && (props.kind === "tts" ? installing === current?.id : status?.models.some((m) => m.id === installing));
  useEffect(() => {
    if (!installing) return;
    let stopped = false;
    let timer: number;
    const poll = async () => {
      const s = await refresh();
      if (stopped) return;
      if (!s || s.installing) {
        timer = window.setTimeout(() => void poll(), 2000);
      } else if (thisModel) {
        setFailed(Boolean(s.last_error));
        if (!s.last_error) toast.show(t("voice.local.installed"), "success");
      }
    };
    timer = window.setTimeout(() => void poll(), 2000);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [installing, thisModel, refresh, toast, t]);

  const ready = Boolean(current?.installed && (props.kind === "tts" || status?.vad_installed));
  const install = async () => {
    if (!client || !current) return;
    setBusy(true);
    setFailed(false);
    try {
      const next = await client.installLocalVoice(current.id);
      setSnapshot({ client, status: next });
      setFailed(Boolean(next.last_error));
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };
  const words = current && whisperText(current.id);
  return (
    <>
      {props.kind === "tts" ? (
        <Switch label={t("voice.offlineReadAloud.title")} checked={props.enabled}
          disabled={!ready && !props.enabled} onChange={props.onEnabled} />
      ) : status?.available ? (
        <select className={formStyles.input} aria-label={t("voice.local.model")} value={current?.id ?? ""}
          onChange={(e) => props.onModel(e.target.value)}>
          {status.models.map((m) => {
            const modelText = whisperText(m.id);
            return <option key={m.id} value={m.id}>{modelText ? t(modelText.label) : m.label}</option>;
          })}
        </select>
      ) : null}
      {status && (!status.available || !current) ? (
        <p>{t(props.kind === "tts" ? "voice.offlineReadAloud.unsupported" : "voice.local.unavailable")}</p>
      ) : null}
      {status?.available && current ? (
        <>
          <p>
            {props.kind === "tts"
              ? t("voice.offlineReadAloud.model", { name: current.label })
              : words ? t(words.description) : current.description}{" "}
            {t("voice.local.details", { size: current.size_mb, license: current.license, host: current.source_host })}{" "}
            <a href={current.license_url} target="_blank" rel="noreferrer">{t("voice.local.licenseLink")}</a>
            {props.kind === "stt" ? (
              <> <a href="https://github.com/snakers4/silero-vad/blob/master/LICENSE" target="_blank" rel="noreferrer">
                {t("voice.offlineModels.vad")}
              </a></>
            ) : null}
          </p>
          <p aria-live="polite">{ready
            ? t(props.kind === "tts" ? "voice.offlineReadAloud.ready" : "voice.local.ready")
            : thisModel ? t("voice.local.downloading")
              : t(props.kind === "tts" ? "voice.offlineReadAloud.notInstalled" : "voice.local.notInstalled")}</p>
          {failed && status.last_error ? <p role="alert">{t("voice.offlineModels.downloadFailed")} {status.last_error}</p> : null}
          {!ready ? (
            <Button size="sm" disabled={busy || installing !== null} onClick={() => void install()}>
              {t("voice.local.download", { size: current.size_mb })}
            </Button>
          ) : null}
        </>
      ) : null}
    </>
  );
}
