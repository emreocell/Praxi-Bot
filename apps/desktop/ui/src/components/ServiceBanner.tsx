import { ClipboardCopy, Loader2, RotateCw } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { startupDiagnostics } from "../api/connection";
import { buildServiceDiagnostic } from "../lib/serviceDiagnostic";
import { useDaemon } from "../state/DaemonContext";
import { useServiceState } from "../state/ServiceState";
import { useToast } from "./Toast";
import styles from "./ServiceBanner.module.css";

/**
 * The single, non-blocking notice about the local service. The live region is always mounted and
 * polite so a screen reader hears the change once, without it stealing focus from a dialog.
 */
export function ServiceBanner() {
  const { t } = useTranslation();
  const toast = useToast();
  const { restartDaemon, phase } = useDaemon();
  const svc = useServiceState();
  const [restarting, setRestarting] = useState(false);

  const restart = async () => {
    setRestarting(true);
    try {
      await restartDaemon(undefined, "retry-service-banner");
    } finally {
      setRestarting(false);
    }
  };
  const copy = async () => {
    const text = buildServiceDiagnostic(await startupDiagnostics(), phase);
    try {
      await navigator.clipboard.writeText(text);
      toast.show(t("service.copied"), "success");
    } catch {
      toast.show(t("service.copyFailed"), "danger");
    }
  };

  const shown = svc.bannerVisible;
  const key = svc.status === "failed" ? "failed" : svc.status === "starting" ? "starting" : "reconnecting";
  return (
    <div role="status" aria-live="polite" aria-atomic="true" data-testid="service-banner-live">
      {shown ? (
        <div className={styles.banner} data-status={svc.status} data-testid="service-banner">
          {svc.status === "failed" ? null : <Loader2 className={styles.spin} aria-hidden="true" />}
          <div className={styles.text}>
            <strong className={styles.title}>{t(`service.${key}.title`)}</strong>
            <span className={styles.hint}>{t(`service.${key}.hint`)}</span>
          </div>
          {svc.canRestart ? (
            <div className={styles.actions}>
              <Button size="sm" variant="primary" disabled={restarting} onClick={() => void restart()} data-testid="service-restart">
                <RotateCw aria-hidden="true" />
                {restarting ? t("errors.restarting") : t("errors.restartService")}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => void copy()} data-testid="service-copy-diagnostic">
                <ClipboardCopy aria-hidden="true" />
                {t("service.copy")}
              </Button>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
