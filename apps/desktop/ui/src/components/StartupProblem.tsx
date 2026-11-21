import { Check, Copy, FolderOpen, RotateCw, ServerOff, ShieldAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { openLogFolder, type StartupDiagnostics } from "../api/connection";
import { humanStartup } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import styles from "./StartupProblem.module.css";

/** How many of the service's last log lines are shown without opening anything. */
const SHOWN_LOG_LINES = 8;

/**
 * Shown when the local service did not answer within ~15 s (or failed): why, when the app knows
 * (exit code, blocked, missing, port in use), the service's last log lines, and the four actions
 * that help (try again, restart, copy the details, open the log folder).
 */
export function StartupProblem({ startup, raw }: { startup: StartupDiagnostics; raw: string }) {
  const { t } = useTranslation();
  const { retry, restartDaemon } = useDaemon();
  const [copied, setCopied] = useState(false);
  const h = humanStartup(startup, raw);
  const lines = startup.log_tail.slice(-SHOWN_LOG_LINES);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(h.details);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard blocked: the log lines stay selectable on the screen.
    }
  };
  return (
    <div className={styles.root} data-testid="startup-problem" data-stage={startup.stage}>
      <span className={styles.icon}>
        <ServerOff aria-hidden="true" />
      </span>
      <h1 className={styles.title}>{h.title}</h1>
      <p className={styles.hint}>{h.hint}</p>
      <p className={styles.waited}>{t("errors.startupPanel.waited", { seconds: Math.round(startup.elapsed_ms / 1000) })}</p>
      {lines.length > 0 ? (
        <>
          <p className={styles.logTitle}>{t("errors.startupPanel.logTitle")}</p>
          <pre className={styles.log} data-testid="startup-log">{lines.join("\n")}</pre>
        </>
      ) : null}
      <div className={styles.actions}>
        <Button variant="secondary" onClick={retry}>
          <RotateCw aria-hidden="true" /> {t("errors.startupPanel.retry")}
        </Button>
        <Button variant="primary" onClick={() => void restartDaemon()}>
          {t("errors.startupPanel.restart")}
        </Button>
        <Button variant="secondary" onClick={() => void restartDaemon(true)} title={t("safeMode.startup.hint")}>
          <ShieldAlert aria-hidden="true" /> {t("safeMode.startup.button")}
        </Button>
        <Button variant="secondary" onClick={() => void copy()}>
          {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
          {copied ? t("errors.startupPanel.copied") : t("errors.startupPanel.copy")}
        </Button>
        <Button variant="secondary" onClick={() => void openLogFolder().catch(() => undefined)}>
          <FolderOpen aria-hidden="true" /> {t("errors.startupPanel.openLogs")}
        </Button>
      </div>
    </div>
  );
}
