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
  testId
}: {
  icon: LucideIcon;
  title: string;
  children: ReactNode;
  actionLabel?: string;
  onAction?: () => void;
  testId?: string;
}) {
  return (
    <div className={styles.root} role="status" data-testid={testId}>
      <Icon className={styles.icon} aria-hidden="true" />
      <p className={styles.title}>{title}</p>
      <p className={styles.body}>{children}</p>
      {actionLabel && onAction ? (
        <Button variant="primary" size="sm" onClick={onAction}>
          {actionLabel}
        </Button>
      ) : null}
    </div>
  );
}
