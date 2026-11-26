import { ChevronDown, TriangleAlert } from "lucide-react";
import type { KeyboardEvent, ReactNode } from "react";
import { splitInlineCode } from "../lib/inlineCode";
import styles from "./SettingsUi.module.css";

/** A description line; a `command` in backticks is drawn as code, never with the raw backticks. */
function Description({ text }: { text: string }) {
  return (
    <p className={styles.rowDesc}>
      {splitInlineCode(text).map((part, i) =>
        part.code ? <code key={i} className={styles.inlineCode}>{part.text}</code> : part.text
      )}
    </p>
  );
}

/** One setting: icon-less title and one-line description on the left, control on the right. */
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
    <div className={[styles.row, stacked ? styles.stacked : ""].filter(Boolean).join(" ")}>
      <div className={styles.rowText}>
        <h3 className={styles.rowTitle}>{title}</h3>
        {description ? <Description text={description} /> : null}
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
  const activeIdx = Math.max(
    0,
    options.findIndex((o) => o.value === value)
  );
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step =
      e.key === "ArrowRight" || e.key === "ArrowDown"
        ? 1
        : e.key === "ArrowLeft" || e.key === "ArrowUp"
          ? -1
          : 0;
    if (!step || options.length === 0) {
      return;
    }
    e.preventDefault();
    const nextIdx = (activeIdx + step + options.length) % options.length;
    onChange(options[nextIdx].value);
    const radios = e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]');
    radios[nextIdx]?.focus();
  };
  return (
    <div
      className={styles.segmented}
      role="radiogroup"
      aria-label={label}
      onKeyDown={onKeyDown}
    >
      {options.map((o, idx) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          tabIndex={idx === activeIdx ? 0 : -1}
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
      title={label}
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

/**
 * A titled card for settings blocks that bring their own body (lists, forms).
 * Icon-less title, one-line plain description, unified surface and spacing.
 * `tone="danger"` marks a danger zone: destructive actions live apart, with a red edge.
 */
export function SettingsCard({
  title,
  description,
  tone = "default",
  testId,
  children
}: {
  title: string;
  description?: string;
  tone?: "default" | "danger";
  testId?: string;
  children: ReactNode;
}) {
  return (
    <section
      className={[styles.card, tone === "danger" ? styles.cardDanger : ""].filter(Boolean).join(" ")}
      data-testid={testId}
    >
      <header className={styles.cardHead}>
        <h3 className={styles.cardTitle}>{title}</h3>
        {description ? <Description text={description} /> : null}
      </header>
      {children}
    </section>
  );
}

/** Unified consequence note and optional "What is this?" popover row beneath a setting or card. */
export function SettingsCardFooter({
  risk,
  action
}: {
  risk?: string;
  action?: ReactNode;
}) {
  if (!risk && !action) return null;
  return (
    <div className={styles.cardFooter}>
      {risk ? (
        <p className={styles.riskNote}>
          <TriangleAlert aria-hidden="true" />
          <span>{risk}</span>
        </p>
      ) : (
        <span />
      )}
      {action}
    </div>
  );
}

/** Collapsible progressive-disclosure section for expert settings on a page. */
export function SettingsDisclosure({
  open,
  count,
  label,
  hint,
  onToggle,
  testId,
  children
}: {
  open: boolean;
  count: number;
  label: string;
  hint?: string;
  onToggle: () => void;
  testId?: string;
  children: ReactNode;
}) {
  return (
    <div className={styles.disclosure}>
      <button
        type="button"
        className={styles.disclosureBtn}
        aria-expanded={open}
        data-testid={testId}
        onClick={onToggle}
      >
        <span className={styles.disclosureLeft}>
          <span className={styles.disclosureTitle}>{label}</span>
          <span className={styles.disclosureCount}>{count}</span>
          {hint && !open ? <span className={styles.disclosureHint}>{hint}</span> : null}
        </span>
        <ChevronDown
          aria-hidden="true"
          className={[styles.disclosureChev, open ? styles.disclosureChevOpen : ""].filter(Boolean).join(" ")}
        />
      </button>
      {open ? <div className={styles.disclosureBody}>{children}</div> : null}
    </div>
  );
}

/** A calm "nothing here yet" line for an empty list. */
export function EmptyNote({ icon, children }: { icon?: ReactNode; children: ReactNode }) {
  return (
    <p className={styles.emptyNote} role="status">
      {icon}
      <span>{children}</span>
    </p>
  );
}
