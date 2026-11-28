import { CircleHelp } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { LearnTerm } from "../lib/settingsPages";
import { openExternal } from "../lib/externalLink";
import { Popover } from "./Popover";
import styles from "./LearnMore.module.css";

const DOCS_BASE = "https://github.com/emreocell/Praxi-Bot/blob/main/docs/guide/";

/** The guide page that explains each term. */
export const LEARN_DOCS: Record<LearnTerm, string> = {
  apiKey: "providers-and-models.md",
  provider: "providers-and-models.md",
  permissions: "chatting-and-approvals.md",
  folders: "security.md",
  memory: "knowledge-embeddings.md",
  safeMode: "troubleshooting.md",
  mcpTrust: "mcp-servers.md"
};

/** Hover: a short wait before it opens, and a grace period so the pointer can move into the popover. */
const HOVER_OPEN_MS = 150;
const HOVER_CLOSE_MS = 250;

/**
 * A small "?" icon in a card header. Its plain-words explanation opens as a popover on hover and on
 * click (Enter / Space from the keyboard); a click keeps it open until Escape, a click outside, or
 * another click on the "?". A hover-open never moves the focus.
 */
export function LearnMore({ term }: { term: LearnTerm }) {
  const { t } = useTranslation();
  // `pinned`: opened by a click or the keyboard; `hover`: opened by the pointer only.
  const [open, setOpen] = useState<"closed" | "hover" | "pinned">("closed");
  const anchor = useRef<HTMLButtonElement | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const id = useId();
  const title = t(`settingsModal.learn.terms.${term}.title`);

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
    if (open === "hover" || open === "closed") timer.current = window.setTimeout(() => setOpen((o) => (o === "hover" ? "closed" : o)), HOVER_CLOSE_MS);
  };

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={styles.trigger}
        aria-expanded={open !== "closed"}
        aria-controls={open !== "closed" ? id : undefined}
        aria-label={t("settingsUi.help.label", { topic: title })}
        title={t("settingsModal.learn.open")}
        data-testid={`learn-${term}`}
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
        <div id={id} className={styles.body} onMouseEnter={hoverIn} onMouseLeave={hoverOut}>
          <h4 className={styles.title}>{title}</h4>
          <p className={styles.text}>{t(`settingsModal.learn.terms.${term}.body`)}</p>
          <button
            type="button"
            className={styles.link}
            onClick={() => openExternal(`${DOCS_BASE}${LEARN_DOCS[term]}`)}
          >
            {t("settingsModal.learn.guide")}
          </button>
        </div>
      </Popover>
    </>
  );
}
