import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { LocalInstallDto } from "../api/types";
import { canInstall, isBusy, percent, phaseKey, sizeLabel } from "../lib/localInstall";
import { useDaemon } from "../state/DaemonContext";
import styles from "./LocalModelsAdvice.module.css";

const POLL_MS = 2000;

/**
 * One-click local model: pick a curated model, Praxi downloads a pinned program and the model
 * (checksum-verified), starts it on this computer only, and the usual local-server detection then
 * offers it as "llama.cpp". Hidden when the daemon cannot answer or this computer has no pinned build.
 */
export function LocalInstallCard() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [data, setData] = useState<LocalInstallDto | null>(null);
  const [failed, setFailed] = useState(false);

  const refresh = useCallback(() => {
    if (!client) {
      return Promise.resolve();
    }
    return client.localInstall().then(
      (d) => {
        setData(d);
        setFailed(false);
      },
      () => setFailed(true)
    );
  }, [client]);

  const busy = data !== null && isBusy(data.state);
  useEffect(() => {
    void refresh();
  }, [refresh]);
  useEffect(() => {
    if (!busy) {
      return;
    }
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [busy, refresh]);

  if (!client || failed || !data || !data.supported) {
    return null;
  }
  const key = phaseKey(data.state);
  const isError = data.state.phase === "failed";
  const running = data.state.phase === "ready";
  const run = (action: Promise<unknown>) => void action.then(refresh, refresh);

  return (
    <section className={styles.box} data-testid="local-install" aria-label={t("localInstall.title")}>
      <h3 className={styles.title}>{t("localInstall.title")}</h3>
      <p className={styles.line}>{t("localInstall.intro")}</p>
      {key ? (
        <p className={styles.line} role={isError ? "alert" : "status"} data-testid="local-install-state">
          {t(`localInstall.${key}`, { percent: percent(data.state) })}
        </p>
      ) : null}
      {busy ? <progress value={percent(data.state)} max={100} aria-label={t("localInstall.title")} /> : null}
      {busy ? (
        <div className={styles.row}>
          <button type="button" className={styles.copy} onClick={() => run(client.stopLocalInstall())}>
            {t("localInstall.cancel")}
          </button>
        </div>
      ) : null}
      {running ? (
        <div className={styles.row}>
          <button type="button" className={styles.copy} onClick={() => run(client.stopLocalInstall())}>
            {t("localInstall.stop")}
          </button>
        </div>
      ) : null}
      {!busy && !running ? (
        <ul className={styles.list}>
          {data.models.map((m) => (
            <li key={m.id} className={styles.row} data-testid={`local-install-${m.id}`}>
              <span>{m.display_name}</span>
              {m.recommended ? <span className={styles.fit}>{t("localInstall.recommended")}</span> : null}
              <span className={styles.fit}>
                {t(
                  m.fit === "comfortable"
                    ? "localInstall.fitFast"
                    : m.fit === "slow"
                      ? "localInstall.fitSlow"
                      : "localInstall.tooLarge"
                )}
              </span>
              {m.disk_ok === false ? <span className={styles.fit}>{t("localInstall.noDisk")}</span> : null}
              <span className={styles.note}>
                {t("localInstall.download", { size: sizeLabel(m.size_bytes), runtime: sizeLabel(data.runtime_size_bytes) })}{" "}
                {t("localInstall.licence", { licence: m.licence })}
              </span>
              {canInstall(m) ? (
                <button
                  type="button"
                  className={styles.copy}
                  data-testid="local-install-start"
                  onClick={() => run(client.startLocalInstall(m.id))}
                >
                  {t("localInstall.install")}
                </button>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      <p className={styles.note}>{t("localInstall.disclosure")}</p>
    </section>
  );
}
