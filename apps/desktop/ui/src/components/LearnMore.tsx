import { CircleHelp } from "lucide-react";
import { useId, useRef, useState } from "react";
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
  safeMode: "troubleshooting.md"
};

/** A small "?" button: a plain-words explanation in a popover, with a link to the guide. */
export function LearnMore({ term }: { term: LearnTerm }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement | null>(null);
  const id = useId();
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={styles.trigger}
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        data-testid={`learn-${term}`}
        onClick={() => setOpen((v) => !v)}
      >
        <CircleHelp aria-hidden="true" />
        <span>{t("settingsModal.learn.open")}</span>
      </button>
      <Popover
        open={open}
        anchor={anchor}
        onClose={() => setOpen(false)}
        role="dialog"
        label={t(`settingsModal.learn.terms.${term}.title`)}
        width={320}
      >
        <div id={id} className={styles.body}>
          <h4 className={styles.title}>{t(`settingsModal.learn.terms.${term}.title`)}</h4>
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
