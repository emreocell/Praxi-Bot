import { Check, Copy, RotateCw, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { useDaemon } from "../state/DaemonContext";
import { useServiceState } from "../state/ServiceState";
import { type ErrorAction, type HumanError, humanError } from "../lib/humanError";
import styles from "./ErrorNotice.module.css";

/**
 * An error in a person's words: what happened, what to do, one action button when it helps,
 * and the raw text behind a "Details" disclosure (copyable). The raw text is never the headline.
 */
export function ErrorNotice({
  error,
  compact = false,
  onAction,
  busy = false
}: {
  /** A ready `HumanError`, or anything thrown (it is described here). */
  error: unknown;
  compact?: boolean;
  /** Handles the button; without it no button is shown. */
  onAction?: (action: ErrorAction) => void;
  busy?: boolean;
}) {
  const { t } = useTranslation();
  const { restartDaemon } = useDaemon();
  const serviceBlocked = useServiceState().blocked;
  const [copied, setCopied] = useState(false);
  const h: HumanError =
    error && typeof error === "object" && "title" in error && "hint" in error && "details" in error
      ? (error as HumanError)
      : humanError(error);
  // Without a handler the only built-in action is restarting the local service.
  const handle =
    onAction ?? ((a: ErrorAction) => (a === "restartService" ? void restartDaemon(undefined, "retry-error-notice") : undefined));
  // "Cannot reach the service" while the app banner already says so: one notice, not two.
  if (h.code === "network" && serviceBlocked) return null;
  const label: Record<ErrorAction, string> = {
    retry: t("errors.retry"),
    reload: t("errors.reload"),
    restartService: busy ? t("errors.restarting") : t("errors.restartService"),
    settings: t("errors.openSettings"),
    fixEngine: t("errors.fixEngine")
  };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(h.details);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard blocked: the text stays selectable in the disclosure.
    }
  };
  return (
    <div className={`${styles.box} ${compact ? styles.compact : ""}`} role="alert" data-testid="error-notice" data-code={h.code}>
      <TriangleAlert className={styles.icon} aria-hidden="true" />
      <div className={styles.text}>
        <p className={styles.title}>{h.title}</p>
        {h.hint ? <p className={styles.hint}>{h.hint}</p> : null}
        {h.details ? (
          <details className={styles.details}>
            <summary>{t("errors.details")}</summary>
            <pre className={styles.raw}>{h.details}</pre>
            <button type="button" className={styles.copy} onClick={() => void copy()}>
              {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
              {copied ? t("errors.copied") : t("errors.copy")}
            </button>
          </details>
        ) : null}
      </div>
      {h.action && (onAction || h.action === "restartService") ? (
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => handle(h.action as ErrorAction)}>
          {h.action === "retry" || h.action === "restartService" ? <RotateCw aria-hidden="true" /> : null}
          {label[h.action]}
        </Button>
      ) : null}
    </div>
  );
}
