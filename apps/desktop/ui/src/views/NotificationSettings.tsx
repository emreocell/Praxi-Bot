import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Card } from "ui";
import formStyles from "../components/Form.module.css";
import { type QuietHours } from "../lib/quietHours";
import type { AppSettings } from "../store/settingsStore";
import styles from "./NotificationSettings.module.css";

type Toggle =
  | "notifyNeedsYou"
  | "notifyLongRun"
  | "notifyFailed"
  | "notifyPrivate"
  | "showAgentRoute";

const NOTIFY_TOGGLES: readonly Toggle[] = [
  "notifyNeedsYou",
  "notifyLongRun",
  "notifyFailed"
];

/** Settings: OS notifications per kind, private mode, and the route-chip debug switch. */
export function NotificationSettings({
  settings,
  onChange
}: {
  settings: Pick<AppSettings, Toggle | "quietHours" | "digestTime">;
  onChange: (patch: Partial<AppSettings>) => void;
}) {
  const { t, i18n } = useTranslation();
  const idBase = useId();
  const q = settings.quietHours;
  const setQuiet = (patch: Partial<QuietHours>) =>
    onChange({ quietHours: { ...q, ...patch } });
  const weekday = (d: number) =>
    new Intl.DateTimeFormat(i18n.language, { weekday: "short" }).format(
      new Date(2024, 0, 7 + d) // 2024-01-07 is a Sunday
    );
  const zones: string[] =
    typeof Intl.supportedValuesOf === "function"
      ? Intl.supportedValuesOf("timeZone")
      : [];

  const row = (key: Toggle) => (
    <label key={key} className={styles.row}>
      <input
        type="checkbox"
        className={styles.check}
        checked={settings[key]}
        onChange={(e) => onChange({ [key]: e.target.checked })}
        data-testid={`settings-${key}`}
      />
      <span className={styles.text}>
        <span className={styles.label}>{t(`settings.notifications.${key}`)}</span>
        <span className={formStyles.hint}>
          {t(`settings.notifications.${key}Hint`)}
        </span>
      </span>
    </label>
  );

  return (
    <Card padding="md" className={styles.card}>
      <div className={styles.info}>
        <h2 className={styles.heading}>{t("settings.notifications.title")}</h2>
        <p className={formStyles.hint}>
          {t("settings.notifications.description")}
        </p>
      </div>
      <div className={styles.group}>{NOTIFY_TOGGLES.map(row)}</div>
      <div className={styles.group}>{row("notifyPrivate")}</div>
      <div className={styles.group} data-testid="quiet-hours">
        <h3 className={styles.subHeading}>{t("settings.quiet.title")}</h3>
        <label className={styles.row}>
          <input
            type="checkbox"
            className={styles.check}
            checked={q.enabled}
            onChange={(e) => setQuiet({ enabled: e.target.checked })}
            data-testid="settings-quiet-enabled"
          />
          <span className={styles.text}>
            <span className={styles.label}>{t("settings.quiet.enabled")}</span>
            <span className={formStyles.hint}>{t("settings.quiet.enabledHint")}</span>
          </span>
        </label>
        {q.enabled ? (
          <>
            <div className={styles.times}>
              <label className={styles.field} htmlFor={`${idBase}-from`}>
                <span className={styles.label}>{t("settings.quiet.from")}</span>
                <input
                  id={`${idBase}-from`}
                  type="time"
                  className={styles.input}
                  value={q.from}
                  onChange={(e) => e.target.value && setQuiet({ from: e.target.value })}
                  data-testid="settings-quiet-from"
                />
              </label>
              <label className={styles.field} htmlFor={`${idBase}-to`}>
                <span className={styles.label}>{t("settings.quiet.to")}</span>
                <input
                  id={`${idBase}-to`}
                  type="time"
                  className={styles.input}
                  value={q.to}
                  onChange={(e) => e.target.value && setQuiet({ to: e.target.value })}
                  data-testid="settings-quiet-to"
                />
              </label>
              <label className={styles.field} htmlFor={`${idBase}-tz`}>
                <span className={styles.label}>{t("settings.quiet.timezone")}</span>
                <select
                  id={`${idBase}-tz`}
                  className={styles.input}
                  value={q.timezone}
                  onChange={(e) => setQuiet({ timezone: e.target.value })}
                  data-testid="settings-quiet-timezone"
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
            <fieldset className={styles.days}>
              <legend className={styles.label}>{t("settings.quiet.days")}</legend>
              {[1, 2, 3, 4, 5, 6, 0].map((d) => (
                <label key={d} className={styles.day}>
                  <input
                    type="checkbox"
                    className={styles.check}
                    checked={q.days.includes(d)}
                    onChange={(e) =>
                      setQuiet({
                        days: e.target.checked
                          ? [...q.days, d].sort()
                          : q.days.filter((x) => x !== d)
                      })
                    }
                    data-testid={`settings-quiet-day-${d}`}
                  />
                  <span className={styles.label}>{weekday(d)}</span>
                </label>
              ))}
            </fieldset>
            <label className={styles.row}>
              <input
                type="checkbox"
                className={styles.check}
                checked={q.allowUrgent}
                onChange={(e) => setQuiet({ allowUrgent: e.target.checked })}
                data-testid="settings-quiet-urgent"
              />
              <span className={styles.text}>
                <span className={styles.label}>{t("settings.quiet.allowUrgent")}</span>
                <span className={formStyles.hint}>{t("settings.quiet.allowUrgentHint")}</span>
              </span>
            </label>
          </>
        ) : null}
      </div>
      <div className={styles.group} data-testid="digest-settings">
        <h3 className={styles.subHeading}>{t("settings.digest.title")}</h3>
        <label className={styles.row}>
          <input
            type="checkbox"
            className={styles.check}
            checked={settings.digestTime !== ""}
            onChange={(e) => onChange({ digestTime: e.target.checked ? "18:00" : "" })}
            data-testid="settings-digest-enabled"
          />
          <span className={styles.text}>
            <span className={styles.label}>{t("settings.digest.enabled")}</span>
            <span className={formStyles.hint}>{t("settings.digest.enabledHint")}</span>
          </span>
        </label>
        {settings.digestTime !== "" ? (
          <label className={styles.field} htmlFor={`${idBase}-digest`}>
            <span className={styles.label}>{t("settings.digest.time")}</span>
            <input
              id={`${idBase}-digest`}
              type="time"
              className={styles.input}
              value={settings.digestTime}
              onChange={(e) => e.target.value && onChange({ digestTime: e.target.value })}
              data-testid="settings-digest-time"
            />
          </label>
        ) : null}
      </div>
      <div className={styles.group}>
        <h3 className={styles.subHeading}>{t("settings.route.title")}</h3>
        {row("showAgentRoute")}
      </div>
    </Card>
  );
}
