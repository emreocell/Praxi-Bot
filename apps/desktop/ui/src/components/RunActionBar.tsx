import type { ComponentType, ReactNode, SVGProps } from "react";

import tipStyles from "./ActionTip.module.css";
import styles from "./RunActionBar.module.css";

/** One compact row for the actions under the latest answer (timeline, remember, workflow, regenerate). */
export function RunActionBar({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className={styles.bar} role="group" aria-label={label} data-testid="run-actions">
      {children}
    </div>
  );
}

/** A small icon button: its name shows right above it on hover / focus and is its accessible name. */
export function IconAction({
  icon: Icon,
  label,
  onClick,
  disabled,
  testId,
  pressed
}: {
  icon: ComponentType<SVGProps<SVGSVGElement>>;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  testId?: string;
  pressed?: boolean;
}) {
  return (
    <button
      type="button"
      className={styles.icon}
      aria-label={label}
      aria-pressed={pressed}
      disabled={disabled}
      data-testid={testId}
      onClick={onClick}
    >
      <Icon aria-hidden="true" />
      <span className={tipStyles.tip} aria-hidden="true">{label}</span>
    </button>
  );
}
