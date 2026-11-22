import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { LocalModelsDto } from "../api/types";
import { copyText } from "../lib/copyText";
import {
  copyablePull,
  fittingModels,
  hardwareFacts,
  probeIsEmpty,
  recommendedModel,
  runtimeState
} from "../lib/localAdvice";
import { useDaemon } from "../state/DaemonContext";
import styles from "./LocalModelsAdvice.module.css";

function CopyCommand({ command }: { command: string }) {
  const { t } = useTranslation();
  const [done, setDone] = useState(false);
  return (
    <>
      <code className={styles.cmd}>{command}</code>
      <button
        type="button"
        className={styles.copy}
        data-testid="local-copy"
        onClick={() => void copyText(command).then((ok) => setDone(ok))}
      >
        {done ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
        {done ? t("localModels.copied") : t("localModels.copy")}
      </button>
    </>
  );
}

/**
 * What this computer can run locally (Ollama, LM Studio), from `GET /v1/local-models`.
 * Best effort: the numbers are estimates, the commands are only shown for copying (Praxi never
 * runs them), and the detected hardware stays on this computer. Fails soft: when the daemon
 * cannot answer, one short note is shown instead.
 */
export function LocalModelsAdvice() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [data, setData] = useState<LocalModelsDto | null | "failed">(null);

  useEffect(() => {
    if (!client) {
      setData("failed");
      return;
    }
    let alive = true;
    client.localModels().then(
      (d) => alive && setData(d),
      () => alive && setData("failed")
    );
    return () => {
      alive = false;
    };
  }, [client]);

  if (data === null) {
    return null;
  }
  if (data === "failed" || probeIsEmpty(data.hardware)) {
    return (
      <section className={styles.box} data-testid="local-advice" aria-label={t("localModels.title")}>
        <h3 className={styles.title}>{t("localModels.title")}</h3>
        <p className={styles.note}>{t("localModels.unknown")}</p>
      </section>
    );
  }

  const f = hardwareFacts(data.hardware);
  const unknown = t("localModels.unknownValue");
  const memory = f.unified
    ? t("localModels.unified", { ram: f.ram ?? unknown })
    : t("localModels.ramGpu", {
        ram: f.ram ?? unknown,
        gpu: f.gpu ? (f.vram ? `${f.gpu} (${f.vram})` : f.gpu) : t("localModels.noGpu")
      });
  const fits = fittingModels(data.advice);
  const best = recommendedModel(data.advice);
  const bestCmd = best ? copyablePull(best) : null;
  const ollama = runtimeState(data, "ollama");
  const ceiling = data.advice.max_comfortable_billions;

  return (
    <section className={styles.box} data-testid="local-advice" aria-label={t("localModels.title")}>
      <h3 className={styles.title}>{t("localModels.title")}</h3>
      <p className={styles.line} data-testid="local-hardware">
        {memory}
        {f.cores !== null ? ` · ${t("localModels.cores", { count: f.cores })}` : ""}
        {f.disk ? ` · ${t("localModels.disk", { size: f.disk })}` : ""}
      </p>

      {best && bestCmd ? (
        <p className={styles.line}>{t("localModels.recommended", { name: best.display_name })}</p>
      ) : (
        <p className={styles.line}>{t("localModels.none")}</p>
      )}
      {fits.length > 0 ? (
        <ul className={styles.list}>
          {fits.map((m) => {
            const cmd = copyablePull(m);
            return (
              <li key={m.model_id} className={styles.row} data-testid={`local-model-${m.model_id}`}>
                <span>{m.display_name}</span>
                <span className={styles.fit}>{t(m.fit === "comfortable" ? "localModels.fitFast" : "localModels.fitSlow")}</span>
                {m.disk_ok === false ? <span className={styles.fit}>{t("localModels.noDisk")}</span> : null}
                {cmd ? <CopyCommand command={cmd} /> : null}
              </li>
            );
          })}
        </ul>
      ) : null}

      {ollama === "notInstalled" ? <p className={styles.line}>{t("localModels.ollamaMissing")}</p> : null}
      {ollama === "notRunning" ? <p className={styles.line}>{t("localModels.ollamaStopped")}</p> : null}
      {ceiling ? <p className={styles.line}>{t("localModels.lmstudio", { size: ceiling })}</p> : null}
      <p className={styles.note}>{t("localModels.bestEffort")}</p>
    </section>
  );
}
