import { ChevronRight, ShieldCheck } from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import styles from "./ChoiceList.module.css";

export interface ChoiceOption<T extends string> {
  value: T;
  label: string;
  /** One short sentence: what this choice does. Details belong in a `MoreInfo` under the list. */
  hint: string;
  /** Marked "Recommended" so a newcomer can pick without reading. */
  recommended?: boolean;
}

/**
 * A single choice as stacked option rows: a radio, a bold name, an optional "Recommended" badge and
 * one line of plain words. Replaces the old "**Name** — long sentence" radio lists in Settings.
 */
export function ChoiceList<T extends string>({
  name,
  label,
  value,
  options,
  onChange,
  disabled = false,
  testIdPrefix
}: {
  /** The radio group name; unique per bot when the same list is shown for several bots. */
  name: string;
  /** Read by screen readers (the card title already says it on screen). */
  label: string;
  value: T | null | undefined;
  options: ReadonlyArray<ChoiceOption<T>>;
  onChange: (value: T) => void;
  disabled?: boolean;
  testIdPrefix?: string;
}) {
  const { t } = useTranslation();
  return (
    <fieldset className={styles.list} disabled={disabled}>
      <legend className={styles.legend}>{label}</legend>
      {options.map((o) => {
        const checked = o.value === value;
        return (
          <label key={o.value} className={[styles.option, checked ? styles.checked : ""].filter(Boolean).join(" ")}>
            <input
              type="radio"
              className={styles.radio}
              name={name}
              checked={checked}
              data-testid={testIdPrefix ? `${testIdPrefix}-${o.value}` : undefined}
              onChange={() => onChange(o.value)}
            />
            <span className={styles.text}>
              <span className={styles.title}>
                {o.label}
                {o.recommended ? (
                  <span className={styles.badge}>
                    <ShieldCheck aria-hidden="true" />
                    {t("choice.recommended")}
                  </span>
                ) : null}
              </span>
              <span className={styles.hint}>{o.hint}</span>
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}

/** "How it works": the details a curious owner may want, folded so the card stays short. */
export function MoreInfo({ label, children, testId }: { label?: string; children: ReactNode; testId?: string }) {
  const { t } = useTranslation();
  return (
    <details className={styles.more} data-testid={testId}>
      <summary className={styles.moreSummary}>
        <ChevronRight aria-hidden="true" className={styles.moreChev} />
        {label ?? t("choice.howItWorks")}
      </summary>
      <div className={styles.moreBody}>{children}</div>
    </details>
  );
}
