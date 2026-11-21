import { ChevronDown } from "lucide-react";
import { type ReactNode, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { isLongMessage } from "../lib/longMessage";
import styles from "../views/ChatView.module.css";

/**
 * The user's own bubble. A very long message is folded to a few lines with a
 * fade and a "Show more" toggle; the full text stays in the DOM (find in chat,
 * copy and screen readers still see all of it).
 */
export function UserBubble({ text, children }: { text: string; children: ReactNode }) {
  const { t } = useTranslation();
  const long = isLongMessage(text);
  const [open, setOpen] = useState(false);
  const id = useId();
  const folded = long && !open;
  return (
    <>
      <div id={id} className={styles.userBubble} data-folded={folded ? "true" : undefined}>
        {children}
      </div>
      {long ? (
        <button
          type="button"
          className={styles.foldToggle}
          aria-expanded={open}
          aria-controls={id}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? t("chat.longMessage.less") : t("chat.longMessage.more")}
          <ChevronDown className={open ? styles.foldOpen : undefined} aria-hidden="true" />
        </button>
      ) : null}
    </>
  );
}
