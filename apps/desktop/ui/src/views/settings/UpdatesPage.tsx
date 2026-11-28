import { getVersion } from "@tauri-apps/api/app";
import { CircleAlert, CircleCheck, CloudDownload, LoaderCircle, RotateCcw } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { SettingGroup, SettingRow, SettingsCard } from "../../components/SettingsUi";
import { PRODUCT_CONFIG } from "../../config/product";
import { formatWhen } from "../../lib/dateFormat";
import {
  type UpdateChannel,
  type UpdateInfo,
  type UpdateStatus,
  checkForUpdate,
  installUpdate,
  rollbackUpdate,
  setUpdateChannel,
  skipUpdate,
  updateErrorKey,
  updateStatus
} from "../../lib/appUpdate";
import { updateHeadline } from "../../lib/updateHeadline";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./UpdatesPage.module.css";

/** A version with a suffix (0.1.0-alpha.3) is a preview build: Stable never offers it updates. */
const isPreviewVersion = (version: string) => version.includes("-");

type Phase = "idle" | "checking" | "installing" | "rollingBack" | "saving";

const HEADLINE_ICON = {
  notSetUp: CircleAlert,
  available: CloudDownload,
  upToDate: CircleCheck,
  unchecked: CircleAlert
} as const;

/**
 * Settings > Updates: the installed version, the channel and the last check in one summary card
 * with one primary action (Check for updates); a found update gets its own card with the release
 * notes and its one primary action (Update now). A build without a signed update feed says so
 * plainly and offers nothing that cannot work.
 */
export function UpdatesPage() {
  const { t, i18n } = useTranslation();
  const { health } = useDaemon();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [appVersion, setAppVersion] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [found, setFound] = useState<UpdateInfo | null>(null);
  const [upToDate, setUpToDate] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void updateStatus(i18n.language).then(setStatus).catch((e: unknown) => setError(t(updateErrorKey(e))));
  }, [i18n.language, t]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    void getVersion().then(setAppVersion).catch(() => undefined);
  }, []);

  const run = async (next: Phase, action: () => Promise<void>) => {
    setPhase(next);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(t(updateErrorKey(e)));
    } finally {
      try {
        setStatus(await updateStatus(i18n.language));
      } catch (e) {
        setError(t(updateErrorKey(e)));
      }
      setPhase("idle");
    }
  };

  const check = () =>
    run("checking", async () => {
      setFound(null);
      setUpToDate(false);
      const info = await checkForUpdate(i18n.language);
      setFound(info);
      setUpToDate(info === null);
    });

  const busy = phase !== "idle";
  const version = status?.currentVersion || appVersion || t("settingsModal.updates.devBuild");
  const headline = status ? updateHeadline(status, found, upToDate) : "unchecked";
  const HeadIcon = HEADLINE_ICON[headline];
  const lastChecked =
    status?.lastChecked == null
      ? t("settingsUi.updates.headline.unchecked")
      : formatWhen(status.lastChecked * 1000, i18n.language);

  return (
    <div className={styles.page} data-testid="updates-page">
      <section className={styles.summary} aria-labelledby="updates-summary-title" data-testid="updates-summary">
        <div className={styles.summaryHead}>
          <div className={styles.versionBlock}>
            <span className={styles.eyebrow}>{t("settingsUi.updates.installed", { app: PRODUCT_CONFIG.appName })}</span>
            <h3 id="updates-summary-title" className={styles.version} data-testid="app-version">
              {version}
            </h3>
          </div>
          <span className={`${styles.badge} ${styles[`badge_${headline}`]}`} role="status" data-testid="updates-headline">
            <HeadIcon aria-hidden="true" />
            {t(`settingsUi.updates.headline.${headline}`)}
          </span>
        </div>

        {status && !status.enabled ? (
          <p className={styles.note} data-testid="updates-not-configured">{t("settingsModal.updates.notConfigured")}</p>
        ) : null}

        <dl className={styles.facts}>
          <div className={styles.fact}>
            <dt>{t("settingsModal.updates.channel.title")}</dt>
            <dd>
              <select
                className={styles.select}
                aria-label={t("settingsModal.updates.channel.title")}
                value={status?.channel ?? "stable"}
                disabled={busy || !status?.enabled}
                onChange={(e) => {
                  const channel = e.target.value as UpdateChannel;
                  void run("saving", async () => {
                    await setUpdateChannel(channel);
                    setFound(null);
                    setUpToDate(false);
                  });
                }}
              >
                <option value="stable">{t("settingsModal.updates.channel.stable")}</option>
                <option value="preview" disabled={!status?.previewAvailable}>
                  {t("settingsModal.updates.channel.preview")}
                </option>
              </select>
            </dd>
          </div>
          <div className={styles.fact}>
            <dt>{t("settingsUi.updates.lastCheck")}</dt>
            <dd data-testid="updates-last-checked">{lastChecked}</dd>
          </div>
        </dl>
        {status?.enabled ? (
          <p className={styles.hint}>
            {status.previewAvailable ? t("settingsModal.updates.channel.description") : t("settingsModal.updates.channel.unavailable")}
          </p>
        ) : null}
        {status?.enabled && status.channel === "stable" && isPreviewVersion(status.currentVersion) ? (
          <p className={styles.hint}>{t("settingsModal.updates.previewBuildHint")}</p>
        ) : null}
        {status?.skippedVersion ? (
          <p className={styles.hint}>{t("settingsModal.updates.skipped", { version: status.skippedVersion })}</p>
        ) : null}

        <div className={styles.actions}>
          <Button
            size="sm"
            variant={found ? "secondary" : "primary"}
            disabled={busy || !status?.enabled}
            data-testid="updates-check"
            onClick={() => void check()}
          >
            {phase === "checking" ? <LoaderCircle className={styles.spin} aria-hidden="true" /> : null}
            {phase === "checking" ? t("settingsModal.updates.check.working") : t("settingsModal.updates.check.action")}
          </Button>
          {headline === "upToDate" && upToDate && !error ? (
            <span className={styles.hint}>{t("settingsModal.updates.upToDate")}</span>
          ) : null}
          {error ? <span className={styles.error} role="alert">{error}</span> : null}
        </div>
      </section>

      {found ? (
        <section className={styles.available} aria-labelledby="updates-available-title" data-testid="updates-available">
          <div className={styles.availableHead}>
            <span className={styles.availableIcon} aria-hidden="true">
              <CloudDownload />
            </span>
            <div>
              <h3 id="updates-available-title" className={styles.availableTitle}>
                {t("settingsModal.updates.available", { version: found.version })}
              </h3>
              {found.date ? (
                <p className={styles.hint}>{t("settingsUi.updates.released", { date: formatWhen(Date.parse(found.date), i18n.language) })}</p>
              ) : null}
            </div>
          </div>
          <h4 className={styles.notesTitle}>{t("settingsUi.updates.notes")}</h4>
          {found.notes ? (
            <div className={styles.notes} data-testid="updates-notes">{found.notes}</div>
          ) : (
            <p className={styles.hint}>{t("settingsUi.updates.noNotes")}</p>
          )}
          <div className={styles.actions}>
            <Button size="sm" variant="primary" disabled={busy} data-testid="updates-install" onClick={() => void run("installing", installUpdate)}>
              {phase === "installing" ? t("settingsModal.updates.install.working") : t("settingsModal.updates.install.action")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() =>
                void run("saving", async () => {
                  await skipUpdate();
                  setFound(null);
                  setUpToDate(false);
                })
              }
            >
              {t("settingsModal.updates.skip")}
            </Button>
          </div>
          <p className={styles.hint}>{t("settingsModal.updates.install.hint")}</p>
        </section>
      ) : null}

      {status?.rollbackAdvised ? (
        <SettingsCard
          tone="danger"
          title={t("settingsModal.updates.rollback.title")}
          description={t("settingsModal.updates.rollback.description", { version: status.currentVersion })}
        >
          <div className={styles.actions}>
            <Button
              size="sm"
              variant="primary"
              disabled={busy || !status.rollbackAvailable}
              onClick={() => void run("rollingBack", rollbackUpdate)}
            >
              <RotateCcw aria-hidden="true" />
              {phase === "rollingBack" ? t("settingsModal.updates.rollback.working") : t("settingsModal.updates.rollback.action")}
            </Button>
          </div>
        </SettingsCard>
      ) : null}

      <SettingsCard title={t("settingsUi.updates.versions")} description={t("settingsUi.updates.versionsDescription")}>
        <SettingGroup>
          <SettingRow title={PRODUCT_CONFIG.appName} description={t("settingsModal.updates.appVersion")}>
            <span className={styles.value}>{appVersion || status?.currentVersion || t("settingsModal.updates.devBuild")}</span>
          </SettingRow>
          <SettingRow title={t("settingsUi.updates.service")} description={t("settingsUi.updates.serviceDescription")}>
            <span className={styles.value} data-testid="daemon-version">{health?.version ?? t("settingsModal.updates.unknown")}</span>
          </SettingRow>
          <SettingRow title={t("settingsModal.updates.buildId")}>
            <span className={styles.value}>{status?.buildId || t("settingsModal.updates.unknown")}</span>
          </SettingRow>
        </SettingGroup>
      </SettingsCard>
    </div>
  );
}
