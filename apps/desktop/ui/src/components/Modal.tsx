import { type ReactNode, useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { isTabStop, returnFocusTarget, trapTarget } from "../lib/focusTrap";
import { appInert } from "../lib/inertStack";
import { layerStack as modalStack } from "../lib/modalStack";
import styles from "./Modal.module.css";

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([disabled])';

/** Only the topmost open layer (dialog or popover) reacts to Esc / Tab. */

/**
 * A centered dialog over a dimmed backdrop: Esc and a backdrop click close it,
 * Tab stays inside, and focus returns to where it was when it closes.
 */
export function Modal({
  open,
  onClose,
  label,
  describedBy,
  size = "lg",
  children
}: {
  open: boolean;
  onClose: () => void;
  /** Accessible name of the dialog. */
  label: string;
  /** Id of the element that explains the dialog (its subtitle or lead text). */
  describedBy?: string;
  size?: "confirm" | "sm" | "md" | "lg" | "apps";
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  // Taken at open time (render), so it follows opening order even within one commit.
  const seqRef = useRef(0);
  if (open && seqRef.current === 0) seqRef.current = modalStack.next();
  if (!open) seqRef.current = 0;
  const closeRef = useRef(onClose);
  useEffect(() => { closeRef.current = onClose; }, [onClose]);

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    const previous = document.activeElement as HTMLElement | null;
    const node = ref.current;
    const seq = seqRef.current;
    modalStack.add(seq);
    appInert.acquire(seq);
    (node?.querySelector<HTMLElement>("[data-autofocus]") ?? node)?.focus({ preventScroll: true });
    const onKey = (e: KeyboardEvent) => {
      if (!modalStack.isTop(seq)) return;
      if (e.key === "Escape") {
        e.stopPropagation();
        closeRef.current();
        return;
      }
      if (e.key === "Tab" && node) {
        // Only real Tab stops: not hidden controls (a `display: none` file input) and not
        // roving-tabindex members that are currently out of the order (inactive tabs).
        const items = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) =>
          isTabStop({
            tabIndex: el.tabIndex,
            rects: el.getClientRects().length,
            inert: el.closest("[inert]") !== null
          })
        );
        const active = document.activeElement as HTMLElement | null;
        const target = trapTarget(items, active, node, e.shiftKey);
        if (target === "none") {
          e.preventDefault();
          node.focus();
        } else if (target) {
          e.preventDefault();
          target.focus();
        }
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      modalStack.remove(seq);
      appInert.release(seq);
      returnFocusTarget(previous, (el) => el.isConnected)?.focus?.();
    };
  }, [open]);

  if (!open) {
    return null;
  }
  return createPortal(
    <div
      className={styles.backdrop}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
    >
      <div
        ref={ref}
        className={[styles.dialog, styles[size]].join(" ")}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        aria-describedby={describedBy}
        tabIndex={-1}
      >
        {children}
      </div>
    </div>,
    document.body
  );
}
