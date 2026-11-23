import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { LocalVoiceStatus } from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";

/** Picker, licence and download button for the offline (on-device) speech models. */
export function LocalVoicePanel({ model, onModel }: { model: string; onModel: (id: string) => void }) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [status, setStatus] = useState<LocalVoiceStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const next = await client?.getLocalVoice();
      if (next) setStatus(next);
      return next ?? null;
    } catch {
      return null;
    }
  }, [client]);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  // While a download runs, look again every two seconds; report a failure once it stops.
  const installing = status?.installing ?? null;
  useEffect(() => {
    if (!installing) return;
    const id = window.setInterval(() => {
      void refresh().then((s) => {
        if (s && !s.installing) {
          toast.show(s.last_error ?? t("voice.local.installed"), s.last_error ? "danger" : "success");
        }
      });
    }, 2000);
    return () => window.clearInterval(id);
  }, [installing, refresh, toast, t]);

  if (!status) return null;
  if (!status.available) {
    return <p>{t("voice.local.unavailable")}</p>;
  }
  const current = status.models.find((m) => m.id === model) ?? status.models[0];
  const install = async () => {
    if (!client || !current) return;
    setBusy(true);
    try {
      setStatus(await client.installLocalVoice(current.id));
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <select
        className={formStyles.input}
        aria-label={t("voice.local.model")}
        value={current?.id ?? ""}
        onChange={(e) => onModel(e.target.value)}
      >
        {status.models.map((m) => (
          <option key={m.id} value={m.id}>
            {m.label}
          </option>
        ))}
      </select>
      {current ? (
        <p>
          {current.description}{" "}
          {t("voice.local.details", { size: current.size_mb, license: current.license, host: current.source_host })}{" "}
          <a href={current.license_url} target="_blank" rel="noreferrer">
            {t("voice.local.licenseLink")}
          </a>
        </p>
      ) : null}
      <p>
        {current?.installed && status.vad_installed
          ? t("voice.local.ready")
          : installing
            ? t("voice.local.downloading")
            : t("voice.local.notInstalled")}
      </p>
      {current && !(current.installed && status.vad_installed) ? (
        <Button size="sm" disabled={busy || installing !== null} onClick={() => void install()}>
          {t("voice.local.download", { size: current.size_mb })}
        </Button>
      ) : null}
    </>
  );
}
