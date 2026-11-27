import { Ban, Check, Eye, PenLine, Scale, SlidersHorizontal, Zap } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import type { PermissionPreset } from "../api/types";
import { ACCESS_LEVELS, accessLevelKey } from "../lib/accessLevels";
import type { PresetChoice } from "../lib/botProfile";
import styles from "./AccessLevelPicker.module.css";

const ICONS: Record<PresetChoice, typeof Eye> = {
  full: Zap,
  "approve-destructive": Scale,
  "read-and-draft": PenLine,
  "read-only": Eye,
  custom: SlidersHorizontal
};

/**
 * How much a bot may do without asking, as one list used everywhere a level is chosen: one
 * row per level (name and its one-line meaning), the chosen one marked, and under the list a
 * short "what it can do / what still stops it" for the chosen level.
 */
export function AccessLevelPicker({
  value,
  onChange,
  label,
  allowCustom = false,
  recommended,
  disabled = false,
  testIdPrefix
}: {
  /** The stored choice; `null` while unknown (nothing is marked). */
  value: PresetChoice | null;
  onChange: (preset: PresetChoice) => void;
  /** The group's accessible name. */
  label: string;
  /** Offer "Custom" (a rule per kind of action) as a fifth row. */
  allowCustom?: boolean;
  recommended?: PermissionPreset;
  disabled?: boolean;
  /** `<prefix>-<preset>` on every row. */
  testIdPrefix: string;
}) {
  const { t } = useTranslation();
  const name = useId();
  const rows: PresetChoice[] = allowCustom ? [...ACCESS_LEVELS, "custom"] : [...ACCESS_LEVELS];
  const list = (key: string) => {
    const items = t(key, { returnObjects: true, defaultValue: [] });
    return Array.isArray(items) ? (items as string[]) : [];
  };
  const chosen = value && value !== "custom" ? value : null;
  return (
    <div className={styles.root} data-testid={`${testIdPrefix}-picker`}>
      <div className={styles.list} role="radiogroup" aria-label={label}>
        {rows.map((p) => {
          const Icon = ICONS[p];
          const active = value === p;
          return (
            <label
              key={p}
              className={[styles.row, active ? styles.active : "", p === "custom" ? styles.custom : ""]
                .filter(Boolean)
                .join(" ")}
              data-level={p}
              data-testid={`${testIdPrefix}-${p}`}
            >
              <input
                type="radio"
                className={styles.radio}
                name={name}
                value={p}
                checked={active}
                disabled={disabled}
                onChange={() => onChange(p)}
              />
              <span className={styles.icon} aria-hidden="true">
                <Icon />
              </span>
              <span className={styles.text}>
                <span className={styles.name}>
                  {t(`${accessLevelKey(p)}.name`)}
                  {p === recommended ? <span className={styles.tag}>{t("accessLevel.recommended")}</span> : null}
                </span>
                <span className={styles.line}>{t(`${accessLevelKey(p)}.line`)}</span>
              </span>
              <span className={styles.dot} aria-hidden="true" />
            </label>
          );
        })}
      </div>
      {chosen ? (
        <details className={styles.more} data-testid={`${testIdPrefix}-more`}>
          <summary>{t("accessLevel.more", { level: t(`${accessLevelKey(chosen)}.name`) })}</summary>
          <div className={styles.columns}>
            <div>
              <p className={styles.columnTitle}>{t("accessLevel.canTitle")}</p>
              <ul className={styles.items}>
                {list(`${accessLevelKey(chosen)}.can`).map((line) => (
                  <li key={line}>
                    <Check className={styles.can} aria-hidden="true" />
                    {line}
                  </li>
                ))}
              </ul>
            </div>
            <div>
              <p className={styles.columnTitle}>{t("accessLevel.stopsTitle")}</p>
              <ul className={styles.items}>
                {list(`${accessLevelKey(chosen)}.stops`).map((line) => (
                  <li key={line}>
                    <Ban className={styles.stops} aria-hidden="true" />
                    {line}
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </details>
      ) : null}
    </div>
  );
}
