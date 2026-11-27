import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import styles from "./MessageActions.module.css";

/**
 * One row under a user or assistant message bubble: the action icons on the left, the time on
 * the right of them. Action buttons stay keyboard-focusable and reveal smoothly on row hover,
 * focus-within, or while an action is active (e.g. "Copied" confirmation).
 *
 * The newest answer of a finished run passes `runActions` (steps, remember, retry): they share the
 * group with the message actions, and the group stays visible, as the run bar under the latest
 * answer always was.
 */
export function MessageActions({
  side,
  meta,
  runActions,
  children
}: {
  side: "bot" | "me";
  /** Persistent subtle metadata such as the message timestamp. */
  meta?: ReactNode;
  /** The finished run's own actions, shown before the message actions of its newest answer. */
  runActions?: ReactNode;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const group = (
    <div
      className={styles.actions}
      role="group"
      aria-label={t("chat.messageActions")}
      data-testid="message-actions"
      data-latest={runActions ? "true" : undefined}
    >
      {runActions}
      {children}
    </div>
  );
  return (
    <div className={`${styles.row} ${side === "me" ? styles.rowUser : styles.rowBot}`}>
      {group}
      {meta}
    </div>
  );
}
