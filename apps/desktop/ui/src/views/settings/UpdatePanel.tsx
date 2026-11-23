import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { SettingRow } from "../../components/SettingsUi";
import {
  type UpdateInfo,
  type UpdateStatus,
  checkForUpdate,
  installUpdate,
  rollbackUpdate,
  updateErrorKey,
  updateStatus
} from "../../lib/appUpdate";
import styles from "./UpdatePanel.module.css";

type Phase = "idle" | "checking" | "installing" | "rollingBack";

/**
 * Check for updates, read what is new, install with one click (the app checks
 * the signature before it installs anything) and go back to the previous
 * version when the new one does not start properly.
 */
export function UpdatePanel() {
  const { t, i18n } = useTranslation();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [found, setFound] = useState<UpdateInfo | null>(null);
  const [upToDate, setUpToDate] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void updateStatus().then(setStatus);
  }, []);

  if (!status) {
    return null;
  }

  const run = async (next: Phase, action: () => Promise<void>) => {
    setPhase(next);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(t(updateErrorKey(e)));
    } finally {
      setPhase("idle");
    }
  };

  const check = () =>
    run("checking", async () => {
      const info = await checkForUpdate(i18n.language);
      setFound(info);
      setUpToDate(info === null);
    });

  if (!status.enabled) {
    return (
      <SettingRow
        title={t("settingsModal.updates.check.title")}
        description={t("settingsModal.updates.notConfigured")}
      />
    );
  }

  const busy = phase !== "idle";
  return (
    <>
      {status.rollbackAdvised ? (
        <SettingRow
          title={t("settingsModal.updates.rollback.title")}
          description={t("settingsModal.updates.rollback.description", {
            version: status.currentVersion
          })}
        >
          <Button
            size="sm"
            variant="primary"
            disabled={busy || !status.rollbackAvailable}
            onClick={() => void run("rollingBack", rollbackUpdate)}
          >
            {phase === "rollingBack"
              ? t("settingsModal.updates.rollback.working")
              : t("settingsModal.updates.rollback.action")}
          </Button>
        </SettingRow>
      ) : null}
      <SettingRow
        title={t("settingsModal.updates.check.title")}
        description={t("settingsModal.updates.check.description")}
        stacked
      >
        <div className={styles.actions}>
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => void check()}>
            {phase === "checking"
              ? t("settingsModal.updates.check.working")
              : t("settingsModal.updates.check.action")}
          </Button>
          {upToDate && !error ? (
            <span className={styles.status} role="status">{t("settingsModal.updates.upToDate")}</span>
          ) : null}
          {error ? <span className={styles.error} role="alert">{error}</span> : null}
        </div>
        {found ? (
          <>
            <p className={styles.status}>
              {t("settingsModal.updates.available", { version: found.version })}
            </p>
            {found.notes ? <pre className={styles.notes}>{found.notes}</pre> : null}
            <div className={styles.actions}>
              <Button
                size="sm"
                variant="primary"
                disabled={busy}
                onClick={() => void run("installing", installUpdate)}
              >
                {phase === "installing"
                  ? t("settingsModal.updates.install.working")
                  : t("settingsModal.updates.install.action")}
              </Button>
            </div>
            <span className={styles.status}>{t("settingsModal.updates.install.hint")}</span>
          </>
        ) : null}
      </SettingRow>
    </>
  );
}
