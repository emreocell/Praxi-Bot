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
import { layerStack } from "../lib/modalStack";
import styles from "./Popover.module.css";

export type PopoverPlacement = "top-start" | "top-end" | "bottom-start" | "bottom-end";

const ITEMS = '[role="menuitem"],[role="option"]';
const FOCUSABLE = 'button:not(:disabled),input:not(:disabled),[href],[tabindex]:not([tabindex="-1"])';
const GAP = 8;
const EDGE = 8;
/** Room a popover wants before it flips to the other side of its anchor. */
const WANTED = 280;

/**
 * Generic popover (card 198-B): drawn in a portal on <body> so it is never clipped by the rail
 * or a glass panel (a `backdrop-filter` ancestor would otherwise trap `position: fixed`).
 * Anchored to `anchor` (follows scrolling, flips when the other side has more room), closes on
 * outside press, Escape and Tab (menus), and hands focus back.  It takes part in the shared
 * layer stack, so inside a dialog only the topmost layer handles Esc / Tab.
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
  /** `first`: first menu item / option (dialogs: first focusable); `none`: the content focuses itself. */
  initialFocus?: "first" | "none";
  children: ReactNode;
}) {
  const pop = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<CSSProperties | null>(null);
  // Taken at open time (render), like Modal, so opening order decides who is on top.
  const seqRef = useRef(0);
  if (open && seqRef.current === 0) seqRef.current = layerStack.next();
  if (!open) seqRef.current = 0;

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
    const above = r.top - GAP - EDGE;
    const below = vh - r.bottom - GAP - EDGE;
    const wantTop = placement.startsWith("top");
    const want = Math.min(p.scrollHeight, WANTED);
    const top = wantTop ? !(above < want && below > above) : below < want && above > below;
    const maxH = `${Math.max(0, top ? above : below)}px`;
    setPos(
      top
        ? { left, bottom: vh - r.top + GAP, ["--pop-max-h" as string]: maxH }
        : { left, top: r.bottom + GAP, ["--pop-max-h" as string]: maxH }
    );
  }, [anchor, placement]);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return undefined;
    }
    place();
    window.addEventListener("resize", place);
    // Capture: catches scrollable containers (a settings pane) as well as the window.
    window.addEventListener("scroll", place, true);
    const ro = typeof ResizeObserver !== "undefined" && pop.current ? new ResizeObserver(place) : null;
    if (ro && pop.current) ro.observe(pop.current);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
      ro?.disconnect();
    };
  }, [open, place]);

  useEffect(() => {
    if (!open) return undefined;
    const seq = seqRef.current;
    layerStack.add(seq);
    const root = pop.current;
    if (initialFocus === "first") {
      const target =
        root?.querySelector<HTMLElement>(ITEMS) ?? (role === "dialog" ? root?.querySelector<HTMLElement>(FOCUSABLE) : null) ?? root;
      target?.focus();
    }
    const onDown = (e: MouseEvent) => {
      if (!layerStack.isTop(seq)) return;
      const target = e.target as Node;
      if (pop.current?.contains(target) || anchor.current?.contains(target)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !layerStack.isTop(seq)) return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
      anchor.current?.focus();
    };
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
      layerStack.remove(seq);
    };
  }, [open, anchor, onClose, initialFocus, role]);

  if (!open) return null;

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const root = pop.current;
    if (!root || !layerStack.isTop(seqRef.current)) return;
    if (e.key === "Tab") {
      if (role === "menu") {
        onClose();
        anchor.current?.focus();
        return;
      }
      const all = [...root.querySelectorAll<HTMLElement>(FOCUSABLE)];
      const first = all[0];
      const last = all[all.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (!first || !last || !active || !root.contains(active) || active === root) {
        e.preventDefault();
        (e.shiftKey ? last : first)?.focus();
      } else if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const target = e.target as HTMLElement;
    // Radio groups (the thinking level) handle their own arrows.
    if (target.closest('[role="radiogroup"]')) return;
    if (target.tagName === "INPUT" && (e.key === "Home" || e.key === "End")) return;
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
      tabIndex={-1}
      style={{ ...(pos ?? { opacity: 0, left: 0, top: 0 }), ...(width ? { width: Math.min(width, window.innerWidth - EDGE * 2) } : null) }}
      onKeyDown={onKeyDown}
    >
      {children}
    </div>,
    document.body
  );
}
