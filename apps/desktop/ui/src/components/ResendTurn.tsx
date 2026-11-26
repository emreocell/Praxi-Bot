import { ChevronRight, History, Info, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { SupersededGroup } from "../lib/resendTurn";
import { IconAction, RunActionBar } from "./RunActionBar";
import styles from "./ResendTurn.module.css";

/** One line before redoing a turn whose run already used tools that act. */
function ActedNotice() {
  const { t } = useTranslation();
  return (
    <p className={styles.notice} role="note" data-testid="resend-acted">
      <Info aria-hidden="true" />
      <span>{t("chat.resend.acted")}</span>
    </p>
  );
}

/**
 * "Regenerate" under the last answer. Every click is the owner's explicit decision; when the
 * previous run used tools that act, it first asks once more with the one-line warning.
 */
export function RegenerateAction({
  usedWriteTools,
  busy,
  failed = false,
  onRegenerate
}: {
  usedWriteTools: boolean;
  busy: boolean;
  /** The run failed with no answer: the action reads "Try again". */
  failed?: boolean;
  onRegenerate: () => void;
}) {
  const { t } = useTranslation();
  const [confirming, setConfirming] = useState(false);
  useEffect(() => {
    if (busy) {
      setConfirming(false);
    }
  }, [busy]);
  const label = t(confirming ? "chat.resend.regenerateAnyway" : failed ? "chat.resend.retry" : "chat.resend.regenerate");
  return (
    <>
      {confirming ? <ActedNotice /> : null}
      {confirming ? (
        <>
          <button
            type="button"
            className={`${styles.action} ${styles.primary}`}
            disabled={busy}
            data-testid="regenerate"
            onClick={() => {
              setConfirming(false);
              onRegenerate();
            }}
          >
            <RefreshCw aria-hidden="true" />
            {label}
          </button>
          <button type="button" className={styles.action} onClick={() => setConfirming(false)}>
            {t("chat.resend.cancel")}
          </button>
        </>
      ) : (
        <IconAction
          icon={RefreshCw}
          label={label}
          disabled={busy}
          testId="regenerate"
          onClick={() => {
            if (usedWriteTools) {
              setConfirming(true);
              return;
            }
            onRegenerate();
          }}
        />
      )}
    </>
  );
}

/** Standalone bar, for a last answer without a finished-run action row. */
export function RegenerateRow(props: { usedWriteTools: boolean; busy: boolean; onRegenerate: () => void }) {
  const { t } = useTranslation();
  return (
    <div data-testid="regenerate-row">
      <RunActionBar label={t("chat.runActions")}>
        <RegenerateAction {...props} />
      </RunActionBar>
    </div>
  );
}

/** Inline editor that replaces the last user bubble while editing. */
export function EditMessage({
  initial,
  usedWriteTools,
  busy,
  onSave,
  onCancel
}: {
  initial: string;
  usedWriteTools: boolean;
  busy: boolean;
  onSave: (text: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [value, setValue] = useState(initial);
  const area = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    const el = area.current;
    if (el) {
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    }
  }, []);
  const ready = value.trim().length > 0 && !busy;
  return (
    <div className={styles.editor} data-testid="edit-message">
      <textarea
        ref={area}
        className={styles.textarea}
        value={value}
        aria-label={t("chat.resend.editLabel")}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && ready) {
            e.preventDefault();
            onSave(value.trim());
          }
        }}
      />
      {usedWriteTools ? <ActedNotice /> : null}
      <div className={styles.buttons}>
        <button type="button" className={styles.action} onClick={onCancel}>
          {t("chat.resend.cancel")}
        </button>
        <button
          type="button"
          className={`${styles.action} ${styles.primary}`}
          disabled={!ready}
          data-testid="edit-save"
          onClick={() => onSave(value.trim())}
        >
          {t("chat.resend.save")}
        </button>
      </div>
    </div>
  );
}

/** A replaced or regenerated stretch of the chat, folded away but still viewable. */
export function SupersededView({ group }: { group: SupersededGroup }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.earlier} data-testid="superseded-group">
      <button
        type="button"
        className={styles.earlierToggle}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <History aria-hidden="true" />
        {t("chat.resend.earlier")}
        <ChevronRight aria-hidden="true" className={open ? styles.chevronOpen : undefined} />
      </button>
      {open ? (
        <div className={styles.earlierBody}>
          {group.messages.map((m) => (
            <p key={m.id} data-role={m.role}>
              {m.content}
            </p>
          ))}
        </div>
      ) : null}
    </div>
  );
}
