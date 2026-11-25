import { Check, ChevronDown } from "lucide-react";
import { useRef, useState, type ReactNode } from "react";
import { Popover } from "./Popover";
import styles from "./TargetPicker.module.css";

export interface TargetPickerOption {
  id: string;
  label: string;
  hint?: string;
  icon?: ReactNode;
}

/**
 * Styled replacement for a native `<select>` (big live viewer target picker): a quiet pill
 * trigger plus the shared Popover with one row per option.  Keyboard handling (arrows, Esc,
 * focus return) comes from the Popover.
 */
export function TargetPicker({
  options,
  value,
  onPick,
  label,
  placeholder,
  icon,
  disabled = false,
  title,
  testId
}: {
  options: readonly TargetPickerOption[];
  value: string;
  onPick: (id: string) => void;
  label: string;
  placeholder: string;
  icon?: ReactNode;
  disabled?: boolean;
  title?: string;
  testId?: string;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const current = options.find((o) => o.id === value);
  const inactive = disabled || options.length === 0;

  return (
    <div className={styles.root}>
      <button
        ref={trigger}
        type="button"
        className={styles.trigger}
        aria-label={`${label}: ${current?.label ?? placeholder}`}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={inactive}
        title={title}
        data-testid={testId}
        data-value={value}
        onClick={() => setOpen((v) => !v)}
      >
        {icon}
        <span className={styles.text}>{current?.label ?? placeholder}</span>
        <ChevronDown aria-hidden="true" />
      </button>
      <Popover
        open={open}
        anchor={trigger}
        onClose={() => setOpen(false)}
        placement="bottom-start"
        label={label}
        width={340}
      >
        {options.map((o) => (
          <button
            key={o.id}
            type="button"
            role="menuitem"
            className={styles.row}
            data-selected={o.id === value ? "true" : undefined}
            onClick={() => {
              setOpen(false);
              trigger.current?.focus();
              if (o.id !== value) onPick(o.id);
            }}
          >
            {o.icon}
            <span className={styles.rowText}>
              <span className={styles.rowLabel}>{o.label}</span>
              {o.hint ? <span className={styles.rowHint}>{o.hint}</span> : null}
            </span>
            {o.id === value ? <Check aria-hidden="true" /> : null}
          </button>
        ))}
      </Popover>
    </div>
  );
}
