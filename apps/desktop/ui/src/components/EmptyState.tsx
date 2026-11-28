import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "ui";
import styles from "./EmptyState.module.css";

/**
 * An empty list or panel: one icon, what this place is for (title), what to do next (one
 * sentence) and at most one primary action. Never a dead end, never an illustration.
 */
export function EmptyState({
  icon: Icon,
  title,
  children,
  actionLabel,
  onAction,
  primary = true,
  testId,
  actionTestId
}: {
  icon: LucideIcon;
  title: string;
  children: ReactNode;
  actionLabel?: string;
  onAction?: () => void;
  /** The one primary button of the view? Pass `false` when the page header already has one. */
  primary?: boolean;
  testId?: string;
  actionTestId?: string;
}) {
  return (
    <div className={styles.root} role="status" data-testid={testId}>
      <span className={styles.tile}>
        <Icon className={styles.icon} aria-hidden="true" />
      </span>
      <p className={styles.title}>{title}</p>
      <p className={styles.body}>{children}</p>
      {actionLabel && onAction ? (
        <Button className={styles.action} variant={primary ? "primary" : "secondary"} size="sm" data-testid={actionTestId} onClick={onAction}>
          {actionLabel}
        </Button>
      ) : null}
    </div>
  );
}
