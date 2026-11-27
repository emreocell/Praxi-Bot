import { useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import type { OwnerNotifySettings } from "../api/types";
import form from "../components/Form.module.css";
import { SwitchField, SwitchGrid } from "../components/SettingsUi";
import { humanLine } from "../lib/humanError";
import { isValidTimezone } from "../lib/quietHours";
import { useDaemon } from "../state/DaemonContext";
import styles from "./NotificationSettings.module.css";

type EventKey = "finished" | "failed" | "blocked" | "progress";
const EVENTS: readonly EventKey[] = ["finished", "failed", "blocked", "progress"];

/** The window as the form edits it; `from`/`to` are always filled so a half-typed time never saves. */
const DEFAULT_WINDOW = { start: "22:00", end: "08:00", timezone: "" };

/**
 * Settings > Notifications: what the daemon pushes on its own to Telegram, Discord and the other
 * channels (a mission or background run that finished, failed, needs you, or passed a milestone),
 * and the quiet hours in which those pushes wait. Saved on the daemon, because it is the daemon
 * (not this window) that talks to the channels. Owner only.
 */
export function ChannelNotifySettings() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const idBase = useId();
  const [settings, setSettings] = useState<OwnerNotifySettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!client) return undefined;
    let cancelled = false;
    client
      .ownerNotifySettings()
      .then((s) => {
        if (!cancelled) setSettings(s);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(humanLine(e));
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  const save = useCallback(
    async (next: OwnerNotifySettings) => {
      if (!client) return;
      const before = settings;
      setSettings(next);
      setBusy(true);
      setError(null);
      try {
        setSettings(await client.putOwnerNotifySettings(next));
      } catch (e) {
        setSettings(before);
        setError(humanLine(e));
      } finally {
        setBusy(false);
      }
    },
    [client, settings]
  );

  if (!settings) {
    return error ? (
      <p role="alert" className={form.error}>
        {error}
      </p>
    ) : null;
  }
  const quiet = settings.quiet_hours ?? null;
  const zones: string[] =
    typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  const setWindow = (patch: Partial<typeof DEFAULT_WINDOW>) => {
    const next = { ...DEFAULT_WINDOW, ...quiet, ...patch };
    if (!isValidTimezone(next.timezone)) return;
    void save({ ...settings, quiet_hours: next });
  };

  return (
    <div className={styles.card} data-testid="channel-notify">
      <div className={styles.group}>
        <SwitchGrid>
          {EVENTS.map((key) => (
            <SwitchField
              key={key}
              checked={settings[key]}
              disabled={busy}
              onChange={(on) => void save({ ...settings, [key]: on })}
              label={t(`settings.channelNotify.${key}`)}
              hint={t(`settings.channelNotify.${key}Hint`)}
              testId={`channel-notify-${key}`}
            />
          ))}
        </SwitchGrid>
      </div>
      <div className={styles.group} data-testid="channel-notify-quiet">
        <h3 className={styles.subHeading}>{t("settings.channelNotify.quietTitle")}</h3>
        <SwitchField
          checked={quiet !== null}
          disabled={busy}
          onChange={(on) => void save({ ...settings, quiet_hours: on ? DEFAULT_WINDOW : null })}
          label={t("settings.channelNotify.quietEnabled")}
          hint={t("settings.channelNotify.quietHint")}
          testId="channel-notify-quiet-enabled"
        />
        {quiet ? (
          <div className={styles.times}>
            <label className={styles.field} htmlFor={`${idBase}-from`}>
              <span className={styles.label}>{t("settings.quiet.from")}</span>
              <input
                id={`${idBase}-from`}
                type="time"
                className={styles.input}
                value={quiet.start}
                disabled={busy}
                onChange={(e) => e.target.value && setWindow({ start: e.target.value })}
                data-testid="channel-notify-from"
              />
            </label>
            <label className={styles.field} htmlFor={`${idBase}-to`}>
              <span className={styles.label}>{t("settings.quiet.to")}</span>
              <input
                id={`${idBase}-to`}
                type="time"
                className={styles.input}
                value={quiet.end}
                disabled={busy}
                onChange={(e) => e.target.value && setWindow({ end: e.target.value })}
                data-testid="channel-notify-to"
              />
            </label>
            <label className={styles.field} htmlFor={`${idBase}-tz`}>
              <span className={styles.label}>{t("settings.quiet.timezone")}</span>
              <select
                id={`${idBase}-tz`}
                className={styles.input}
                value={quiet.timezone ?? ""}
                disabled={busy}
                onChange={(e) => setWindow({ timezone: e.target.value })}
                data-testid="channel-notify-timezone"
              >
                <option value="">{t("settings.quiet.timezoneSystem")}</option>
                {zones.map((z) => (
                  <option key={z} value={z}>
                    {z}
                  </option>
                ))}
              </select>
            </label>
          </div>
        ) : null}
      </div>
      <p className={form.hint}>{t("settings.channelNotify.approvalsNote")}</p>
      {error ? (
        <p role="alert" className={form.error}>
          {error}
        </p>
      ) : null}
    </div>
  );
}
