import { Check, Monitor } from "lucide-react";
import { useTranslation } from "react-i18next";
import { THEME_NAMES, type ThemeMode } from "ui";
import styles from "./ThemePicker.module.css";

export function ThemePicker({ value, onChange }: { value: ThemeMode; onChange: (mode: ThemeMode) => void }) {
  const { t } = useTranslation();
  const selected = value === "dark" ? "obsidian" : value === "light" ? "clay" : value;
  return (
    <div className={styles.root} role="group" aria-label={t("settingsModal.general.theme.title")}>
      <div className={styles.grid}>
        {THEME_NAMES.map((name) => (
          <button key={name} type="button" className={styles.option} aria-pressed={selected === name} onClick={() => onChange(name)}>
            <span className={styles.preview} data-theme-preview={name}>
              <span className={styles.sidebar}><i /><i /><i /></span>
              <span className={styles.canvas}><i /><i /><b /><i /></span>
              <span className={styles.swatch} />
            </span>
            <span className={styles.label}>{name[0].toUpperCase() + name.slice(1)}{selected === name ? <Check /> : null}</span>
          </button>
        ))}
      </div>
      <button type="button" className={styles.system} aria-pressed={selected === "system"} onClick={() => onChange("system")}>
        <Monitor /><span>{t("settingsModal.general.theme.system")}</span>{selected === "system" ? <Check /> : null}
      </button>
    </div>
  );
}
