import type { ReactNode } from "react";
import styles from "./SettingsUi.module.css";

/** One setting: title and description on the left, the control on the right. */
export function SettingRow({
  title,
  description,
  children,
  stacked = false
}: {
  title: string;
  description?: string;
  children?: ReactNode;
  /** Put the control under the text (wide controls). */
  stacked?: boolean;
}) {
  return (
    <div className={[styles.row, stacked ? styles.stacked : ""].join(" ")}>
      <div className={styles.rowText}>
        <h3 className={styles.rowTitle}>{title}</h3>
        {description ? <p className={styles.rowDesc}>{description}</p> : null}
      </div>
      {children ? <div className={styles.rowControl}>{children}</div> : null}
    </div>
  );
}

/** Rounded container that stacks `SettingRow`s with hairline dividers. */
export function SettingGroup({ children }: { children: ReactNode }) {
  return <div className={styles.group}>{children}</div>;
}

/** A pill-shaped single choice. */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label
}: {
  value: T;
  options: ReadonlyArray<{ value: T; label: string; testId?: string }>;
  onChange: (v: T) => void;
  label: string;
}) {
  return (
    <div className={styles.segmented} role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          data-testid={o.testId}
          className={[
            styles.segment,
            o.value === value ? styles.segmentActive : ""
          ]
            .filter(Boolean)
            .join(" ")}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** An on / off switch. */
export function Switch({
  checked,
  onChange,
  label,
  disabled = false
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={[styles.switch, checked ? styles.switchOn : ""]
        .filter(Boolean)
        .join(" ")}
      onClick={() => onChange(!checked)}
    >
      <span className={styles.thumb} />
    </button>
  );
}
