import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { useDaemon } from "../state/DaemonContext";
import { healthStripMode } from "../lib/health";
import styles from "./HealthIndicator.module.css";

type Level = "ok" | "wait" | "bad";

/** Daemon health in the sidebar header: dot + label (+ retry when it failed). */
export function HealthIndicator() {
  const { t } = useTranslation();
  const { phase, health, eventsConnected, error, conn, retry, restartDaemon } =
    useDaemon();

  let level: Level;
  let label: string;
  if (phase === "connecting") {
    level = "wait";
    label = t("health.connecting");
  } else if (phase === "error") {
    level = "bad";
    label = t("health.failed");
  } else if (!health) {
    level = "bad";
    label = t("health.offline");
  } else if (!eventsConnected) {
    level = "wait";
    label = t("health.reconnecting");
  } else {
    level = "ok";
    label = t("health.connected");
  }

  const title = health
    ? t("health.details", {
        version: health.version,
        uptime: health.uptime_secs,
        mode: conn?.fake_provider ? t("health.fakeProvider") : ""
      })
    : (error ?? label);

  const mode = healthStripMode(level, Boolean(conn?.fake_provider));
  if (mode === "hidden") return null;
  if (mode === "tag") {
    return (
      <div className={`${styles.root} ${styles.fakeOnly}`} role="status" title={t("health.fakeProvider")}>
        <span className={styles.tag}>{t("health.fakeProvider")}</span>
      </div>
    );
  }

  return (
    <div className={styles.root} role="status" title={title}>
      <span
        className={[styles.dot, styles[level]].join(" ")}
        aria-hidden="true"
      />
      <span className={styles.label}>{label}</span>
      {conn?.fake_provider ? <span className={styles.tag}>{t("health.fakeProvider")}</span> : null}
      {phase === "error" || (phase === "ready" && !health) ? (
        <Button
          size="sm"
          variant="ghost"
          onClick={() => (phase === "error" ? retry() : void restartDaemon())}
        >
          {t("health.retry")}
        </Button>
      ) : null}
    </div>
  );
}
