import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject
} from "react";
import { createPortal } from "react-dom";
import styles from "./Popover.module.css";

export type PopoverPlacement = "top-start" | "top-end" | "bottom-start" | "bottom-end";

const ITEMS = '[role="menuitem"],[role="option"]';
const FOCUSABLE = 'button:not(:disabled),input:not(:disabled),[href],[tabindex]:not([tabindex="-1"])';
const GAP = 8;
const EDGE = 8;

/**
 * Generic popover (card 198-B): drawn in a portal on <body> so it is never clipped by the rail
 * or a glass panel (a `backdrop-filter` ancestor would otherwise trap `position: fixed`).
 * Anchored to `anchor`, closes on outside press, Escape and Tab (menus), and hands focus back.
 */
export function Popover({
  open,
  anchor,
  onClose,
  placement = "top-start",
  role = "menu",
  label,
  className,
  width,
  initialFocus = "first",
  children
}: {
  open: boolean;
  anchor: RefObject<HTMLElement | null>;
  onClose: () => void;
  placement?: PopoverPlacement;
  role?: "menu" | "dialog";
  label: string;
  className?: string;
  /** Fixed width in px; the popover never exceeds the window. */
  width?: number;
  /** `first`: first menu item / option; `none`: leave focus where the content puts it. */
  initialFocus?: "first" | "none";
  children: ReactNode;
}) {
  const pop = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<CSSProperties | null>(null);

  const place = useCallback(() => {
    const a = anchor.current;
    const p = pop.current;
    if (!a || !p) return;
    const r = a.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const w = p.offsetWidth;
    const end = placement.endsWith("end");
    const left = Math.max(EDGE, Math.min(end ? r.right - w : r.left, vw - w - EDGE));
    const top = placement.startsWith("top");
    setPos(
      top
        ? { left, bottom: vh - r.top + GAP, ["--pop-max-h" as string]: `${Math.max(160, r.top - GAP - EDGE)}px` }
        : { left, top: r.bottom + GAP, ["--pop-max-h" as string]: `${Math.max(160, vh - r.bottom - GAP - EDGE)}px` }
    );
  }, [anchor, placement]);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return undefined;
    }
    place();
    window.addEventListener("resize", place);
    const ro = typeof ResizeObserver !== "undefined" && pop.current ? new ResizeObserver(place) : null;
    if (ro && pop.current) ro.observe(pop.current);
    return () => {
      window.removeEventListener("resize", place);
      ro?.disconnect();
    };
  }, [open, place]);

  useEffect(() => {
    if (!open) return undefined;
    if (initialFocus === "first") {
      pop.current?.querySelector<HTMLElement>(ITEMS)?.focus();
    }
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (pop.current?.contains(target) || anchor.current?.contains(target)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
      anchor.current?.focus();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open, anchor, onClose, initialFocus]);

  if (!open) return null;

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const root = pop.current;
    if (!root) return;
    if (e.key === "Tab") {
      if (role === "menu") {
        onClose();
        anchor.current?.focus();
        return;
      }
      const all = [...root.querySelectorAll<HTMLElement>(FOCUSABLE)];
      const first = all[0];
      const last = all[all.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last?.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first?.focus();
      }
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const inField = (e.target as HTMLElement).tagName === "INPUT";
    if (inField && (e.key === "Home" || e.key === "End")) return;
    const items = [...root.querySelectorAll<HTMLElement>(ITEMS)];
    if (items.length === 0) return;
    e.preventDefault();
    const i = items.indexOf(document.activeElement as HTMLElement);
    const down = e.key === "ArrowDown";
    const next =
      e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : i < 0 ? (down ? 0 : items.length - 1) : (i + (down ? 1 : -1) + items.length) % items.length;
    items[next]?.focus();
  };

  return createPortal(
    <div
      ref={pop}
      className={[styles.popover, className ?? ""].filter(Boolean).join(" ")}
      role={role}
      aria-label={label}
      aria-modal={role === "dialog" ? false : undefined}
      style={{ ...(pos ?? { opacity: 0, left: 0, top: 0 }), ...(width ? { width: Math.min(width, window.innerWidth - EDGE * 2) } : null) }}
      onKeyDown={onKeyDown}
    >
      {children}
    </div>,
    document.body
  );
}
