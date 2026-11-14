import { type ReactNode, useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { createModalStack } from "../lib/modalStack";
import styles from "./Modal.module.css";

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Only the topmost open dialog reacts to Esc / Tab (nested dialogs). */
const modalStack = createModalStack();

/**
 * A centered dialog over a dimmed backdrop: Esc and a backdrop click close it,
 * Tab stays inside, and focus returns to where it was when it closes.
 */
export function Modal({
  open,
  onClose,
  label,
  size = "lg",
  children
}: {
  open: boolean;
  onClose: () => void;
  /** Accessible name of the dialog. */
  label: string;
  size?: "md" | "lg";
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
    const appRoot = document.getElementById("root");
    if (appRoot) appRoot.inert = true;
    (node?.querySelector<HTMLElement>("[data-autofocus]") ?? node)?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (!modalStack.isTop(seq)) return;
      if (e.key === "Escape") {
        e.stopPropagation();
        closeRef.current();
        return;
      }
      if (e.key === "Tab" && node) {
        const items = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)];
        if (items.length === 0) {
          e.preventDefault();
          return;
        }
        const first = items[0];
        const last = items[items.length - 1];
        const active = document.activeElement;
        if (e.shiftKey && (active === first || active === node)) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && active === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      modalStack.remove(seq);
      if (appRoot && ![...document.querySelectorAll('[aria-modal="true"]')].some(dialog => dialog !== node)) appRoot.inert = false;
      previous?.focus?.();
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
        className={[styles.dialog, size === "md" ? styles.md : styles.lg].join(
          " "
        )}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
      >
        {children}
      </div>
    </div>,
    document.body
  );
}
