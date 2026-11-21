import { Check, CircleAlert, CircleCheck, Info, TriangleAlert, X } from "lucide-react";
import { type HTMLAttributes, type ReactNode, type RefObject, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import styles from "./Dialog.module.css";
import { Modal } from "./Modal";
import { useScrollEdges } from "./useScrollEdges";

/** Title row of a bot dialog: optional leading art, title + subtitle, the close button. */
export function DialogHeader({
  title,
  subtitle,
  subtitleId,
  leading,
  actions,
  onClose,
  closeDisabled = false
}: {
  title: string;
  subtitle?: string;
  /** Id for the subtitle, so the dialog can name it as its description. */
  subtitleId?: string;
  leading?: ReactNode;
  /** Extra buttons before the close button. */
  actions?: ReactNode;
  onClose: () => void;
  closeDisabled?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <header className={styles.header}>
      {leading}
      <div className={styles.headerText}>
        <h2 className={styles.title}>{title}</h2>
        {subtitle ? <p id={subtitleId} className={styles.subtitle}>{subtitle}</p> : null}
      </div>
      {actions}
      <button
        type="button"
        className={styles.iconButton}
        title={t("common.close")}
        aria-label={t("common.close")}
        disabled={closeDisabled}
        onClick={onClose}
      >
        <X aria-hidden="true" />
      </button>
    </header>
  );
}

/**
 * The one scrolling area of a dialog, with a soft fade at an edge that has more content behind
 * it.  `className` replaces the default body spacing for dialogs that lay out their own.
 */
export function DialogBody({
  className,
  testId,
  scrollRef,
  children,
  ...rest
}: {
  className?: string;
  testId?: string;
  /** The scroll element, for callers that reset or read its position. */
  scrollRef?: RefObject<HTMLDivElement | null>;
  children: ReactNode;
} & Pick<HTMLAttributes<HTMLDivElement>, "role" | "id" | "aria-labelledby" | "inert">) {
  const own = useRef<HTMLDivElement | null>(null);
  const ref = scrollRef ?? own;
  const edges = useScrollEdges(ref);
  return (
    <div className={styles.bodyWrap}>
      <div ref={ref} className={className ?? styles.body} data-testid={testId} {...rest}>
        {children}
      </div>
      <span className={styles.edgeTop} data-on={edges.top} aria-hidden="true" />
      <span className={styles.edgeBottom} data-on={edges.bottom} aria-hidden="true" />
    </div>
  );
}

/** The footer that stays put while the body scrolls. */
export function DialogFooter({
  note,
  noteTone = "info",
  noteId,
  children
}: {
  /** Status text on the left ("Unsaved changes"). */
  note?: ReactNode;
  noteTone?: "info" | "warning" | "danger";
  noteId?: string;
  children: ReactNode;
}) {
  return (
    <footer className={styles.footer}>
      {note ? (
        <p
          className={[
            styles.footerNote,
            noteTone === "warning" ? styles.footerNoteWarn : "",
            noteTone === "danger" ? styles.footerNoteDanger : ""
          ]
            .filter(Boolean)
            .join(" ")}
          id={noteId}
          role="status"
        >
          {note}
        </p>
      ) : null}
      {children}
    </footer>
  );
}

const CALLOUT_ICON = {
  info: Info,
  warning: TriangleAlert,
  danger: CircleAlert,
  success: CircleCheck
} as const;

/** An inline note: what is going on and, when it matters, why. */
export function Callout({
  tone = "info",
  title,
  children,
  role
}: {
  tone?: "info" | "warning" | "danger" | "success";
  title?: string;
  children?: ReactNode;
  role?: "status" | "alert";
}) {
  const Icon = CALLOUT_ICON[tone];
  const toneClass =
    tone === "warning"
      ? styles.calloutWarning
      : tone === "danger"
        ? styles.calloutDanger
        : tone === "success"
          ? styles.calloutSuccess
          : "";
  return (
    <div className={[styles.callout, toneClass].filter(Boolean).join(" ")} role={role}>
      <Icon aria-hidden="true" />
      <div className={styles.calloutText}>
        {title ? <p className={styles.calloutTitle}>{title}</p> : null}
        {children ? <div className={styles.calloutBody}>{children}</div> : null}
      </div>
    </div>
  );
}

/** A labelled form field with its hint / error line and an optional character counter. */
export function Field({
  label,
  htmlFor,
  hint,
  error,
  count,
  max,
  children
}: {
  label: string;
  htmlFor?: string;
  hint?: ReactNode;
  error?: ReactNode;
  count?: number;
  max?: number;
  children: ReactNode;
}) {
  return (
    <div className={styles.field}>
      <div className={styles.fieldTop}>
        {htmlFor ? (
          <label className={styles.label} htmlFor={htmlFor}>
            {label}
          </label>
        ) : (
          <span className={styles.label}>{label}</span>
        )}
        {count !== undefined && max !== undefined ? (
          <span
            className={[styles.counter, count > max ? styles.counterOver : ""]
              .filter(Boolean)
              .join(" ")}
            aria-hidden="true"
          >
            {count}/{max}
          </span>
        ) : null}
      </div>
      {children}
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : hint ? (
        <p className={styles.hint}>{hint}</p>
      ) : null}
    </div>
  );
}

/** The drawn box of a custom checkbox (the real `<input>` stays in the DOM, visually hidden). */
export function CheckMark({ checked }: { checked: boolean }) {
  return (
    <span className={[styles.check, checked ? styles.checkOn : ""].filter(Boolean).join(" ")} aria-hidden="true">
      <Check />
    </span>
  );
}

/**
 * A small confirmation above the dialog that asked.  Esc and the backdrop cancel; the safe
 * choice takes focus, so Enter never destroys anything by accident.
 */
export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel,
  cancelLabel,
  tone = "default",
  busy = false,
  confirmDisabled = false,
  error,
  onConfirm,
  onCancel
}: {
  open: boolean;
  title: string;
  body?: ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  tone?: "default" | "danger";
  busy?: boolean;
  confirmDisabled?: boolean;
  error?: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={busy ? () => {} : onCancel} label={title} size="confirm">
      <div className={styles.root} data-testid="confirm-dialog">
        <div className={styles.confirmBody}>
          <h2 className={styles.confirmTitle}>{title}</h2>
          {body ? <div className={styles.confirmText}>{body}</div> : null}
          {error ? (
            <p className={styles.error} role="alert">
              {error}
            </p>
          ) : null}
        </div>
        <footer className={`${styles.footer} ${styles.confirmFooter}`}>
          <Button variant="ghost" disabled={busy} data-autofocus onClick={onCancel}>
            {cancelLabel ?? t("common.cancel")}
          </Button>
          <Button
            variant={tone === "danger" ? "danger" : "primary"}
            disabled={busy || confirmDisabled}
            data-testid="confirm-dialog-accept"
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </footer>
      </div>
    </Modal>
  );
}

export { styles as dialogStyles };
