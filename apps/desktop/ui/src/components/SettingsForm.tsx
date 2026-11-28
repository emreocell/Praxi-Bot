import { CircleHelp } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import formStyles from "./Form.module.css";
import { Popover } from "./Popover";
import styles from "./SettingsForm.module.css";

const HOVER_OPEN_MS = 150;
const HOVER_CLOSE_MS = 250;

/** A "?" beside a field label; long help text lives in the popover, not under the input. */
export function FieldHelp({ title, body, testId }: { title: string; body: string; testId?: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState<"closed" | "hover" | "pinned">("closed");
  const anchor = useRef<HTMLButtonElement | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const id = useId();

  useEffect(() => () => window.clearTimeout(timer.current), []);
  const close = useCallback(() => {
    window.clearTimeout(timer.current);
    setOpen("closed");
  }, []);
  const hoverIn = () => {
    window.clearTimeout(timer.current);
    if (open === "closed") timer.current = window.setTimeout(() => setOpen("hover"), HOVER_OPEN_MS);
  };
  const hoverOut = () => {
    window.clearTimeout(timer.current);
    if (open === "hover" || open === "closed") {
      timer.current = window.setTimeout(() => setOpen((o) => (o === "hover" ? "closed" : o)), HOVER_CLOSE_MS);
    }
  };

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={styles.helpTrigger}
        aria-expanded={open !== "closed"}
        aria-controls={open !== "closed" ? id : undefined}
        aria-label={t("settingsUi.help.label", { topic: title })}
        title={t("settingsModal.learn.open")}
        data-testid={testId}
        onClick={() => {
          window.clearTimeout(timer.current);
          setOpen((o) => (o === "pinned" ? "closed" : "pinned"));
        }}
        onMouseEnter={hoverIn}
        onMouseLeave={hoverOut}
      >
        <CircleHelp aria-hidden="true" />
      </button>
      <Popover
        open={open !== "closed"}
        anchor={anchor}
        onClose={close}
        placement="bottom-start"
        role="dialog"
        label={title}
        width={320}
        initialFocus={open === "pinned" ? "first" : "none"}
      >
        <div id={id} className={styles.helpBody} onMouseEnter={hoverIn} onMouseLeave={hoverOut}>
          <p>{body}</p>
        </div>
      </Popover>
    </>
  );
}

/** One settings field: short on-screen hint, optional popover for the rest, optional narrow width. */
export function FormField({
  id,
  label,
  hint,
  helpTitle,
  helpBody,
  width = "full",
  children
}: {
  id: string;
  label: string;
  hint?: string;
  helpTitle?: string;
  helpBody?: string;
  width?: "full" | "md" | "sm";
  children: ReactNode;
}) {
  const widthClass = width === "sm" ? styles.fieldSm : width === "md" ? styles.fieldMd : "";
  return (
    <div className={[styles.field, widthClass].filter(Boolean).join(" ")}>
      <div className={styles.labelRow}>
        <label className={styles.label} htmlFor={id}>
          {label}
        </label>
        {helpBody ? <FieldHelp title={helpTitle ?? label} body={helpBody} testId={`${id}-help`} /> : null}
      </div>
      {children}
      {hint ? <p className={styles.lineHint}>{hint}</p> : null}
    </div>
  );
}

export type CardStatusTone = "ok" | "warn" | "bad" | "muted";

/** Explicit Save plus a short status line (and optional fix action) for cards that do not auto-save. */
export function CardSaveBar({
  saveLabel,
  onSave,
  saving,
  disabled,
  statusText,
  statusTone = "muted",
  statusDetail,
  action,
  testId
}: {
  saveLabel: string;
  onSave: () => void;
  saving?: boolean;
  disabled?: boolean;
  statusText?: string;
  statusTone?: CardStatusTone;
  statusDetail?: string;
  action?: ReactNode;
  testId?: string;
}) {
  const toneClass =
    statusTone === "ok"
      ? styles.statusOk
      : statusTone === "warn"
        ? styles.statusWarn
        : statusTone === "bad"
          ? styles.statusBad
          : "";
  return (
    <div className={styles.cardBar} data-testid={testId}>
      <div className={styles.cardBarActions}>
        <Button size="sm" disabled={disabled || saving} onClick={onSave}>
          {saveLabel}
        </Button>
      </div>
      {statusText ? (
        <div className={[styles.status, toneClass].filter(Boolean).join(" ")} role="status">
          <span>{statusText}</span>
          {statusDetail ? <span>{statusDetail}</span> : null}
          {action ? <div className={styles.statusAction}>{action}</div> : null}
        </div>
      ) : null}
    </div>
  );
}

export { formStyles };