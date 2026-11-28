import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { isReducedMotionPreferred } from "ui";
import styles from "./ApprovalDock.module.css";

/** One thing the dock can show: a single approval card, or the card that answers several at once. */
export interface DockPage {
  key: string;
  /** A line above the card when it belongs elsewhere (another chat of the bot). */
  caption?: string | null;
  /**
   * `active`: this is the page on screen (it alone answers the Enter / Esc keys, and its buttons show
   * which key answers them).
   */
  render: (active: boolean) => ReactNode;
}

/** A little over `--motion-duration-exit`: how long the answered dock stays mounted to fade out. */
const EXIT_MS = 160;

/**
 * The one place where a run's approvals are asked, docked directly above the composer (the
 * composer squares its top corners while this is open). Compact: one card at a time, a counter
 * with previous / next when several wait. The card shows its own keys (Enter on Allow, Esc on Deny)
 * inside its buttons. It is a self-contained piece: the host passes the pages,
 * it owns nothing about how a card decides. After the answer the run's own step line is the trace.
 */
export function ApprovalDock({ pages: live }: { pages: readonly DockPage[] }) {
  const { t } = useTranslation();
  const [index, setIndex] = useState(0);
  // After the last approval is answered the dock stays mounted for one short exit animation (it fades
  // and sinks into the composer).  While it leaves it is inert: no key and no click reaches the old card.
  const last = useRef<readonly DockPage[]>(live);
  const [, tick] = useState(0);
  const open = live.length > 0;
  if (open) last.current = live;
  useEffect(() => {
    if (open || last.current.length === 0) return undefined;
    // No exit animation (OS setting or the app's own switch): the dock leaves at once.
    const still = document.documentElement.hasAttribute("data-reduce-motion") || isReducedMotionPreferred();
    const id = window.setTimeout(() => {
      last.current = [];
      tick((n) => n + 1);
    }, still ? 0 : EXIT_MS);
    return () => window.clearTimeout(id);
  }, [open]);
  const pages = open ? live : last.current;
  if (pages.length === 0) {
    return null;
  }
  const closing = !open;
  const at = Math.min(index, pages.length - 1);
  const page = pages[at]!;
  return (
    <section
      className={styles.dock}
      data-state={closing ? "closing" : "open"}
      inert={closing}
      aria-hidden={closing || undefined}
      role="region"
      aria-label={t("approvalDock.label")}
      data-testid="approval-dock"
      data-approval-dock=""
      data-count={pages.length}
    >
      {pages.length > 1 ? (
        <div className={styles.stack} data-testid="approval-dock-counter">
          <span className={styles.counter} role="status">
            {t("approvalDock.counter", { current: at + 1, total: pages.length })}
          </span>
          <button
            type="button"
            className={styles.step}
            disabled={at === 0}
            aria-label={t("approvalDock.previous")}
            title={t("approvalDock.previous")}
            onClick={() => setIndex(at - 1)}
          >
            <ChevronLeft size={16} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={styles.step}
            disabled={at === pages.length - 1}
            aria-label={t("approvalDock.next")}
            title={t("approvalDock.next")}
            onClick={() => setIndex(at + 1)}
          >
            <ChevronRight size={16} aria-hidden="true" />
          </button>
        </div>
      ) : null}
      {page.caption ? <p className={styles.caption} title={page.caption}>{page.caption}</p> : null}
      <div className={styles.page} key={page.key}>
        {page.render(!closing)}
      </div>
    </section>
  );
}
