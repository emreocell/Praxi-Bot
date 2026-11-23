import { CircleCheck, ExternalLink, LoaderCircle, PlugZap, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import {
  type ConnectionReportLike,
  connectionDocsUrl,
  detailsText,
  normalizeOutcome
} from "../lib/connectionTest";
import { openExternal } from "../lib/externalLink";
import { humanLine } from "../lib/humanError";
import styles from "./ConnectionTest.module.css";

/**
 * The result of a connection test in a person's words: what happened, one concrete next step,
 * a link to the matching guide section, and the technical text (HTTP status, service code)
 * behind a "Details" disclosure. The daemon already redacted that text.
 */
export function ConnectionResult({ report }: { report: ConnectionReportLike }) {
  const { t, i18n } = useTranslation();
  const outcome = normalizeOutcome(report.outcome);
  const ok = outcome === "ok";
  const details = detailsText(report);
  const Icon = ok ? CircleCheck : TriangleAlert;
  return (
    <div
      className={`${styles.box} ${ok ? styles.ok : ""}`}
      role={ok ? "status" : "alert"}
      data-testid="connection-result"
      data-outcome={outcome}
    >
      <Icon className={styles.icon} aria-hidden="true" />
      <div className={styles.text}>
        <p className={styles.title}>{t(`connectionTest.outcome.${outcome}.title`)}</p>
        <p className={styles.hint}>{t(`connectionTest.outcome.${outcome}.next`)}</p>
        {!ok ? (
          <button
            type="button"
            className={styles.link}
            data-testid="connection-docs"
            onClick={() => openExternal(connectionDocsUrl(i18n.language, outcome))}
          >
            {t("connectionTest.learnMore")}
            <ExternalLink aria-hidden="true" />
          </button>
        ) : null}
        {details ? (
          <details className={styles.details}>
            <summary>{t("connectionTest.details")}</summary>
            <pre className={styles.raw} data-testid="connection-details">
              {details}
            </pre>
          </details>
        ) : null}
      </div>
    </div>
  );
}

/**
 * A "Test connection" button with its result underneath. `run` asks the daemon (no secret is
 * sent from here); a failed request is shown as an unknown result with a plain explanation.
 */
export function ConnectionTestButton({
  run,
  label,
  testId,
  disabled = false,
  onResult
}: {
  run: () => Promise<ConnectionReportLike>;
  label?: string;
  testId?: string;
  disabled?: boolean;
  onResult?: (report: ConnectionReportLike) => void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<ConnectionReportLike | null>(null);
  const start = async () => {
    setBusy(true);
    setReport(null);
    let next: ConnectionReportLike;
    try {
      next = await run();
    } catch (e) {
      next = { target: "", outcome: "unknown", details: humanLine(e) };
    }
    setReport(next);
    setBusy(false);
    onResult?.(next);
  };
  return (
    <div className={styles.wrap}>
      <Button
        size="sm"
        variant="secondary"
        disabled={busy || disabled}
        data-testid={testId ?? "connection-test"}
        onClick={() => void start()}
      >
        {busy ? <LoaderCircle className={styles.spin} aria-hidden="true" /> : <PlugZap aria-hidden="true" />}
        {busy ? t("connectionTest.testing") : (label ?? t("connectionTest.button"))}
      </Button>
      {report ? <ConnectionResult report={report} /> : null}
    </div>
  );
}
