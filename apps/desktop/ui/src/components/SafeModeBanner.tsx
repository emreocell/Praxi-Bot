import { ShieldAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { safeModeView } from "../lib/safeMode";
import { useDaemon } from "../state/DaemonContext";
import styles from "./SafeModeBanner.module.css";

/**
 * Shown for as long as the local service runs in safe mode: tools, schedules, channels and apps
 * are off, chat answers without tools. The only way out is the owner restarting the service
 * normally from here (nothing was deleted or changed, so everything comes back).
 */
export function SafeModeBanner() {
  const { t } = useTranslation();
  const { health, conn, restartDaemon, phase } = useDaemon();
  const view = safeModeView(health, conn);
  if (!view.active) {
    return null;
  }
  return (
    <div className={styles.root} role="status" data-testid="safe-mode-banner">
      <ShieldAlert className={styles.icon} aria-hidden="true" />
      <span className={styles.text}>
        <strong>{t("safeMode.banner.title")}</strong> {t("safeMode.banner.body")}
      </span>
      {view.canLeave ? (
        <Button
          size="sm"
          variant="secondary"
          disabled={phase === "connecting"}
          onClick={() => void restartDaemon(false)}
        >
          {t("safeMode.banner.leave")}
        </Button>
      ) : (
        <span className={styles.hint}>{t("safeMode.banner.leaveManual")}</span>
      )}
    </div>
  );
}
