import { Check, Copy } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { copyText, visibleMessageText } from "../lib/copyText";
import tipStyles from "./ActionTip.module.css";
import styles from "./MessageActions.module.css";

const COPIED_MS = 1800;

/**
 * Copy action for a chat message: copies the user-visible text (plain text or
 * Markdown, never internal/tool JSON) and displays a brief "Copied" confirmation.
 */
export function CopyButton({ text }: { text: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const visible = visibleMessageText(text);
  if (!visible) {
    return null;
  }

  const label = t(copied ? "chat.messageCopied" : "chat.copyMessage");
  return (
    <>
      <button
        type="button"
        className={styles.copyBtn}
        aria-label={label}
        data-copied={copied ? "true" : undefined}
        data-testid="copy-message"
        onClick={() => {
          void copyText(visible).then((ok) => {
            if (!ok) {
              return;
            }
            setCopied(true);
            window.clearTimeout(timer.current);
            timer.current = window.setTimeout(() => setCopied(false), COPIED_MS);
          });
        }}
      >
        {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
        {copied ? <span className={styles.copiedLabel}>{t("chat.messageCopied")}</span> : null}
        <span className={tipStyles.tip} aria-hidden="true">{label}</span>
      </button>
      <span className={styles.srOnly} role="status" aria-live="polite">
        {copied ? t("chat.messageCopied") : ""}
      </span>
    </>
  );
}
