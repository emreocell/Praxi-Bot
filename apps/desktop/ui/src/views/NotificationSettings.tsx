import { useTranslation } from "react-i18next";
import { Card } from "ui";
import formStyles from "../components/Form.module.css";
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
  settings: Pick<AppSettings, Toggle>;
  onChange: (patch: Partial<AppSettings>) => void;
}) {
  const { t } = useTranslation();

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
      <div className={styles.group}>
        <h3 className={styles.subHeading}>{t("settings.route.title")}</h3>
        {row("showAgentRoute")}
      </div>
    </Card>
  );
}
